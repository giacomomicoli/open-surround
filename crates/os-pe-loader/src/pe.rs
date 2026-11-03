//! Parsing PE32+ (x86-64): header, sezioni, directory dei dati, rilocazioni, import, export,
//! TLS e risorse. Solo lettura: niente interpretazione, il caricamento è in `loader`.

use anyhow::{anyhow, bail, Result};

pub const IMAGE_DIRECTORY_ENTRY_EXPORT: usize = 0;
pub const IMAGE_DIRECTORY_ENTRY_IMPORT: usize = 1;
pub const IMAGE_DIRECTORY_ENTRY_RESOURCE: usize = 2;
pub const IMAGE_DIRECTORY_ENTRY_EXCEPTION: usize = 3;
pub const IMAGE_DIRECTORY_ENTRY_BASERELOC: usize = 5;
pub const IMAGE_DIRECTORY_ENTRY_TLS: usize = 9;
pub const IMAGE_DIRECTORY_ENTRY_LOAD_CONFIG: usize = 10;
pub const IMAGE_DIRECTORY_ENTRY_IAT: usize = 12;

pub const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
pub const IMAGE_SCN_MEM_READ: u32 = 0x4000_0000;
pub const IMAGE_SCN_MEM_WRITE: u32 = 0x8000_0000;

/// Directory dei dati (RVA, dimensione).
#[derive(Clone, Copy, Default)]
pub struct DataDirectory {
    pub virtual_address: u32,
    pub size: u32,
}

#[derive(Clone)]
pub struct Section {
    pub name: String,
    pub virtual_size: u32,
    pub virtual_address: u32,
    pub size_of_raw_data: u32,
    pub pointer_to_raw_data: u32,
    pub characteristics: u32,
}

impl Section {
    pub fn is_writable(&self) -> bool {
        self.characteristics & IMAGE_SCN_MEM_WRITE != 0
    }
    pub fn is_executable(&self) -> bool {
        self.characteristics & IMAGE_SCN_MEM_EXECUTE != 0
    }
}

/// Voce della tabella dei rilocamenti: tipo (0 = riempimento, 10 = DIR64) e offset nella pagina.
#[derive(Clone, Copy)]
pub struct Reloc {
    pub rva: u32,
    pub typ: u16,
}

pub struct ImportDescriptor {
    pub dll: String,
    /// (nome o ordinale, RVA dello slot IAT da riempire).
    pub imports: Vec<(ImportName, u32)>,
}

#[derive(Clone, Debug)]
pub enum ImportName {
    Name(String),
    Ordinal(u32),
}

impl ImportName {
    pub fn describe(&self) -> String {
        match self {
            ImportName::Name(n) => n.clone(),
            ImportName::Ordinal(o) => format!("#{o}"),
        }
    }
}

pub struct TlsDirectory {
    pub start_address_of_raw_data: u32,
    pub end_address_of_raw_data: u32,
    pub address_of_index: u32,
    pub address_of_callbacks: u32,
    pub size_of_zero_fill: u32,
    pub callbacks: Vec<u32>,
}

pub struct Pe {
    pub image_base: u64,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub address_of_entry_point: u32,
    pub dll_characteristics: u16,
    pub sections: Vec<Section>,
    pub directories: [DataDirectory; 16],
}

impl Pe {
    pub fn parse(data: &[u8]) -> Result<Pe> {
        if data.len() < 0x40 {
            bail!("file troppo corto per un PE");
        }
        if &data[0..2] != b"MZ" {
            bail!("missing MZ");
        }
        let e_lfanew = u32_at(data, 0x3c)? as usize;
        if e_lfanew + 24 + 240 > data.len() || &data[e_lfanew..e_lfanew + 4] != b"PE\0\0" {
            bail!("intestazione PE non valida a {e_lfanew:#x}");
        }
        let machine = u16_at(data, e_lfanew + 4)?;
        if machine != 0x8664 {
            bail!("solo x86-64 (machine {machine:#x})");
        }
        let size_of_optional_header = u16_at(data, e_lfanew + 20)? as usize;
        let opt = e_lfanew + 24;
        let magic = u16_at(data, opt)?;
        if magic != 0x20b {
            bail!("non è PE32+ (magic {magic:#x})");
        }
        let address_of_entry_point = u32_at(data, opt + 16)?;
        let image_base = u64_at(data, opt + 24)?;
        let size_of_image = u32_at(data, opt + 56)?;
        let size_of_headers = u32_at(data, opt + 60)?;
        let dll_characteristics = u16_at(data, opt + 70)?;
        let number_of_rva_and_sizes = u32_at(data, opt + 108)? as usize;
        let mut directories = [DataDirectory::default(); 16];
        for (i, dir) in directories.iter_mut().enumerate().take(number_of_rva_and_sizes.min(16)) {
            *dir = DataDirectory {
                virtual_address: u32_at(data, opt + 112 + 8 * i)?,
                size: u32_at(data, opt + 112 + 8 * i + 4)?,
            };
        }

        let number_of_sections = u16_at(data, e_lfanew + 6)? as usize;
        let sec_table = opt + size_of_optional_header;
        if sec_table + 40 * number_of_sections > data.len() {
            bail!("tabella sezioni fuori dal file");
        }
        let mut sections = Vec::with_capacity(number_of_sections);
        for i in 0..number_of_sections {
            let off = sec_table + 40 * i;
            let name_end = data[off..off + 8].iter().position(|&b| b == 0).unwrap_or(8);
            sections.push(Section {
                name: String::from_utf8_lossy(&data[off..off + name_end]).into_owned(),
                virtual_size: u32_at(data, off + 8)?,
                virtual_address: u32_at(data, off + 12)?,
                size_of_raw_data: u32_at(data, off + 16)?,
                pointer_to_raw_data: u32_at(data, off + 20)?,
                characteristics: u32_at(data, off + 36)?,
            });
        }
        Ok(Pe { image_base, size_of_image, size_of_headers, address_of_entry_point, dll_characteristics, sections, directories })
    }

    /// RVA → offset nel file (None se fuori da ogni sezione e dagli header).
    pub fn rva_to_offset(&self, data: &[u8], rva: u32) -> Option<usize> {
        if (rva as usize) < self.size_of_headers as usize {
            return Some(rva as usize);
        }
        for s in &self.sections {
            let start = s.virtual_address;
            let end = start + s.virtual_size.max(s.size_of_raw_data);
            if rva >= start && rva < end {
                let in_section = (rva - start) as usize;
                if in_section < s.size_of_raw_data as usize {
                    return Some(s.pointer_to_raw_data as usize + in_section);
                }
                // BSS/zero fill: offset virtuale, nessun byte nel file.
                return None;
            }
        }
        if (rva as usize) < data.len() {
            Some(rva as usize)
        } else {
            None
        }
    }

    pub fn relocations<'a>(&'a self, data: &'a [u8]) -> impl Iterator<Item = Reloc> + 'a {
        let dir = self.directories[IMAGE_DIRECTORY_ENTRY_BASERELOC];
        let size = dir.size as usize;
        let end = dir.virtual_address as usize + size;
        let mut offset = dir.virtual_address as usize;
        let mut exhausted = false;
        std::iter::from_fn(move || {
            if exhausted || offset == 0 {
                return None;
            }
            let page_rva = match u32_at(data, offset) {
                Ok(v) => v,
                Err(_) => {
                    exhausted = true;
                    return None;
                }
            };
            let block_size = match u32_at(data, offset + 4) {
                Ok(v) => v,
                Err(_) => {
                    exhausted = true;
                    return None;
                }
            };
            if block_size < 8 || offset + block_size as usize > end {
                exhausted = true;
                return None;
            }
            let n = ((block_size - 8) / 2) as usize;
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                let e = match u16_at(data, offset + 8 + 2 * i) {
                    Ok(v) => v,
                    Err(_) => {
                        exhausted = true;
                        return None;
                    }
                };
                let (typ, off) = (e >> 12, e & 0xfff);
                if typ != 0 {
                    out.push(Reloc { rva: page_rva + off as u32, typ });
                }
            }
            offset += block_size as usize;
            Some(out.into_iter())
        })
        .flatten()
    }

    pub fn import_descriptors(&self, data: &[u8]) -> Result<Vec<ImportDescriptor>> {
        let dir = self.directories[IMAGE_DIRECTORY_ENTRY_IMPORT];
        let mut out = Vec::new();
        if dir.virtual_address == 0 {
            return Ok(out);
        }
        let base_off = self.rva_to_offset(data, dir.virtual_address).ok_or_else(|| anyhow!("import dir non mappata"))?;
        let mut d = 0;
        loop {
            let off = base_off + d;
            let name_rva = u32_at(data, off + 12)?;
            if name_rva == 0 {
                break;
            }
            let first_thunk = u32_at(data, off + 16)?;
            let orig_first_thunk = u32_at(data, off)?;
            let thunk_rva = if orig_first_thunk != 0 { orig_first_thunk } else { first_thunk };
            let dll = cstr_at(data, self.rva_to_offset(data, name_rva).ok_or_else(|| anyhow!("nome DLL import non mappato"))?);
            let mut imports = Vec::new();
            let mut t = 0;
            loop {
                let slot_rva = thunk_rva + 8 * t;
                let toff = self
                    .rva_to_offset(data, slot_rva)
                    .ok_or_else(|| anyhow!("thunk IAT non mappato ({} slot {t})", dll))?;
                let value = u64_at(data, toff)?;
                if value == 0 {
                    break;
                }
                let name = if value & (1 << 63) != 0 {
                    ImportName::Ordinal((value & 0xffff) as u32)
                } else {
                    let hint_off = self
                        .rva_to_offset(data, value as u32)
                        .ok_or_else(|| anyhow!("import per nome non mappato ({} slot {t})", dll))?;
                    ImportName::Name(cstr_at(data, hint_off + 2))
                };
                imports.push((name, first_thunk + 8 * t));
                t += 1;
            }
            out.push(ImportDescriptor { dll, imports });
            d += 20;
        }
        Ok(out)
    }

    pub fn tls_directory(&self, data: &[u8]) -> Result<Option<TlsDirectory>> {
        let dir = self.directories[IMAGE_DIRECTORY_ENTRY_TLS];
        if dir.virtual_address == 0 {
            return Ok(None);
        }
        let off = self
            .rva_to_offset(data, dir.virtual_address)
            .ok_or_else(|| anyhow!("directory TLS non mappata"))?;
        // I campi della directory TLS sono VA a 64 bit (comprese dell'ImageBase): qui
        // diventano RVA per l'uso del loader.
        let to_rva = |va: u64| -> Result<u32> {
            let rva = va
                .checked_sub(self.image_base)
                .filter(|r| *r <= u32::MAX as u64)
                .ok_or_else(|| anyhow!("VA TLS {va:#x} fuori dall'immagine (base {:#x})", self.image_base))?;
            Ok(rva as u32)
        };
        let start = to_rva(u64_at(data, off)?)?;
        let end = to_rva(u64_at(data, off + 8)?)?;
        let index = to_rva(u64_at(data, off + 16)?)?;
        let callbacks = to_rva(u64_at(data, off + 24)?)?;
        let zero_fill = u32_at(data, off + 32)?;
        let mut cbs = Vec::new();
        if callbacks != 0 {
            let mut i = 0;
            loop {
                let toff = self
                    .rva_to_offset(data, callbacks + 8 * i)
                    .ok_or_else(|| anyhow!("callback TLS non mappato"))?;
                let cb = u64_at(data, toff)?;
                if cb == 0 {
                    break;
                }
                cbs.push(to_rva(cb)?);
                i += 1;
            }
        }
        Ok(Some(TlsDirectory {
            start_address_of_raw_data: start,
            end_address_of_raw_data: end,
            address_of_index: index,
            address_of_callbacks: callbacks,
            size_of_zero_fill: zero_fill,
            callbacks: cbs,
        }))
    }

    /// Nome → RVA della funzione esportata.
    pub fn exports(&self, data: &[u8]) -> Result<std::collections::HashMap<String, u32>> {
        let mut out = std::collections::HashMap::new();
        let dir = self.directories[IMAGE_DIRECTORY_ENTRY_EXPORT];
        if dir.virtual_address == 0 {
            return Ok(out);
        }
        let off = self
            .rva_to_offset(data, dir.virtual_address)
            .ok_or_else(|| anyhow!("directory export non mappata"))?;
        let n = u32_at(data, off + 24)? as usize;
        let names_rva = u32_at(data, off + 32)?;
        let addr_rva = u32_at(data, off + 28)?;
        let ords_rva = u32_at(data, off + 36)?;
        for i in 0..n {
            // aonames[i] è un puntatore al nome: prima lo si legge, poi lo si segue.
            let name_ptr_off = self
                .rva_to_offset(data, names_rva + 4 * i as u32)
                .ok_or_else(|| anyhow!("nome export non mappato"))?;
            let name_rva = u32_at(data, name_ptr_off)?;
            let name_off = self
                .rva_to_offset(data, name_rva)
                .ok_or_else(|| anyhow!("nome export {name_rva:#x} non mappato"))?;
            let ordinal = u16_at(data, self.rva_to_offset(data, ords_rva + 2 * i as u32).ok_or_else(|| anyhow!("ordinale export non mappato"))?)?
                as usize;
            let func_rva = u32_at(data, self.rva_to_offset(data, addr_rva + 4 * ordinal as u32).ok_or_else(|| anyhow!("indirizzo export non mappato"))?)?;
            out.insert(cstr_at(data, name_off), func_rva);
        }
        Ok(out)
    }
}

pub fn u16_at(data: &[u8], off: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(data.get(off..off + 2).ok_or_else(|| anyhow!("lettura oltre la fine (u16 a {off:#x})"))?.try_into().unwrap()))
}
pub fn u32_at(data: &[u8], off: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(data.get(off..off + 4).ok_or_else(|| anyhow!("lettura oltre la fine (u32 a {off:#x})"))?.try_into().unwrap()))
}
pub fn u64_at(data: &[u8], off: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(data.get(off..off + 8).ok_or_else(|| anyhow!("lettura oltre la fine (u64 a {off:#x})"))?.try_into().unwrap()))
}

fn cstr_at(data: &[u8], off: usize) -> String {
    let end = data[off..].iter().position(|&b| b == 0).unwrap_or(data.len() - off);
    String::from_utf8_lossy(&data[off..off + end]).into_owned()
}

/// Cerca una risorsa per id di tipo e id di nome (il modulo DAP usa 255/254) nell'albero a
/// tre livelli della directory delle risorse. `res_file_off` è l'offset nel file dell'inizio
/// della sezione `.rsrc`: gli offset interni dell'albero sono relativi a quella base, mentre
/// `OffsetToData` della voce foglia è un RVA dell'immagine.
/// Ritorna (RVA dei dati, dimensione).
pub fn find_resource(data: &[u8], res_file_off: usize, type_id: u32, name_id: u32) -> Option<(u32, u32)> {
    /// Voci (id, offset) di una directory; le voci con nome a stringa sono gestite come id
    /// non corrispondenti (DAP cerca solo per id).
    fn entries(data: &[u8], dir: usize) -> Vec<(u32, u32)> {
        let n = |off: usize| u16::from_le_bytes([data.get(off).copied().unwrap_or(0), data.get(off + 1).copied().unwrap_or(0)]) as usize;
        (0..n(dir + 12) + n(dir + 14))
            .filter_map(|i| {
                let at = |o: usize| data.get(o).copied().unwrap_or(0);
                let id = u32::from_le_bytes([at(dir + 16 + 8 * i), at(dir + 16 + 8 * i + 1), at(dir + 16 + 8 * i + 2), at(dir + 16 + 8 * i + 3)]);
                let off = u32::from_le_bytes([at(dir + 16 + 8 * i + 4), at(dir + 16 + 8 * i + 5), at(dir + 16 + 8 * i + 6), at(dir + 16 + 8 * i + 7)]);
                Some((id, off))
            })
            .collect()
    }

    for (tid, toff) in entries(data, res_file_off) {
        if tid != type_id || toff & 0x8000_0000 == 0 {
            continue;
        }
        let name_dir = res_file_off + (toff & 0x7fff_ffff) as usize;
        for (nid, noff) in entries(data, name_dir) {
            if nid != name_id || noff & 0x8000_0000 == 0 {
                continue;
            }
            let lang_dir = res_file_off + (noff & 0x7fff_ffff) as usize;
            for (_, loff) in entries(data, lang_dir) {
                if loff & 0x8000_0000 != 0 {
                    continue; // sottolivello non previsto
                }
                let leaf = res_file_off + loff as usize;
                let rva = u32::from_le_bytes([
                    data.get(leaf).copied()?,
                    data.get(leaf + 1).copied()?,
                    data.get(leaf + 2).copied()?,
                    data.get(leaf + 3).copied()?,
                ]);
                let size = u32::from_le_bytes([
                    data.get(leaf + 4).copied()?,
                    data.get(leaf + 5).copied()?,
                    data.get(leaf + 6).copied()?,
                    data.get(leaf + 7).copied()?,
                ]);
                return Some((rva, size));
            }
        }
    }
    None
}
