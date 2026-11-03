//! Caricamento di un'immagine PE32+ in memoria: mappa le sezioni, applica le rilocazioni,
//! riempie la IAT con gli shim (vedi `shims`), registra il TLS del modulo, chiama DllMain.
//!
//! Il modulo resta mappato per tutta la vita del processo: non c'è unload.

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{bail, Context, Result};

use crate::pe::{self, Pe};
use crate::shims;
use crate::teb;

/// Un'immagine caricata. `HMODULE` di Windows = indirizzo base.
pub struct Image {
    pub base: usize,
    pub size: usize,
    pub path: PathBuf,
    /// nome esportato → indirizzo assoluto
    exports: HashMap<String, usize>,
    /// Directory delle risorse (offset nel file della sezione .rsrc + file intero tenuto qui).
    resource_file: Vec<u8>,
    resource_off: usize,
    /// DllMain, per le notifiche DLL_THREAD_ATTACH/DETACH.
    entry: usize,
    /// False dopo DisableThreadLibraryCalls (DAP 7.3.7 la chiama nel suo DllMain).
    thread_calls: AtomicBool,
    _mapping: Mapping,
}

pub const DLL_THREAD_ATTACH: u32 = 2;
pub const DLL_THREAD_DETACH: u32 = 3;

struct Mapping(usize, usize);
impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.0 as *mut c_void, self.1);
        }
    }
}

static IMAGES: LazyLock<Mutex<Vec<Arc<Image>>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn images() -> &'static Mutex<Vec<Arc<Image>>> {
    &IMAGES
}

impl Image {
    /// Carica `path` ed esegue DllMain(DLL_PROCESS_ATTACH). Il thread corrente deve poter
    /// eseguire codice del modulo: se non ha ancora un TEB, viene installato qui.
    pub fn load(path: &Path) -> Result<Arc<Image>> {
        let data = std::fs::read(path).with_context(|| format!("lettura {}", path.display()))?;
        let pe = Pe::parse(&data)?;
        teb::install();

        let size = (pe.size_of_image as usize).next_multiple_of(0x1000);
        // Prova prima la base preferita (il kernel sceglie un indirizzo vicino se è occupata);
        // le rilocazioni vengono comunque applicate, quindi una base diversa va bene lo stesso.
        let hint = if pe.image_base != 0 { pe.image_base as usize } else { 0 };
        let raw = unsafe {
            libc::mmap(
                hint as *mut c_void,
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            bail!("mmap dell'immagine ({size} byte) fallita");
        }
        let base = raw as usize;

        unsafe {
            // Header e sezioni.
            let image = base as *mut u8;
            std::ptr::copy_nonoverlapping(data.as_ptr(), image, pe.size_of_headers as usize);
            for s in &pe.sections {
                let dst = image.add(s.virtual_address as usize);
                std::ptr::write_bytes(dst, 0, s.virtual_size as usize);
                if s.size_of_raw_data > 0 {
                    std::ptr::copy_nonoverlapping(
                        data.as_ptr().add(s.pointer_to_raw_data as usize),
                        dst,
                        (s.size_of_raw_data as usize).min(s.virtual_size as usize),
                    );
                }
            }

            // Rilocazioni DIR64 (delta = base reale - base preferita).
            let delta = (base as u64).wrapping_sub(pe.image_base) as i64;
            if delta != 0 {
                for r in pe.relocations(&data) {
                    match r.typ {
                        10 => {
                            // Un .reloc corrotto non deve scrivere fuori dall'immagine.
                            if r.rva as usize + 8 > size {
                                bail!("rilocazione fuori dall'immagine: rva {:#x} (SizeOfImage {size:#x})", r.rva);
                            }
                            let slot = image.add(r.rva as usize) as *mut u64;
                            slot.write_unaligned(slot.read_unaligned().wrapping_add(delta as u64));
                        }
                        0 => {}
                        t => bail!("tipo di rilocazione non supportato: {t} (rva {:#x})", r.rva),
                    }
                }
            }

            // Import: ogni slot IAT riceve l'indirizzo dello shim.
            for desc in pe.import_descriptors(&data)? {
                for (name, iat_rva) in desc.imports {
                    let addr = shims::resolve(&desc.dll, &name);
                    let slot = image.add(iat_rva as usize) as *mut u64;
                    match addr {
                        Some(a) => *slot = a as u64,
                        None => {
                            // Meglio fallire forte e subito durante lo sviluppo: uno slot nullo
                            // cadrebbe in modo oscuro dentro il modulo.
                            *slot = shims::missing_stub(desc.dll.clone(), name) as u64;
                        }
                    }
                }
            }

            // TLS del modulo: indice nella matrice per-thread e blocco per questo thread.
            if let Some(tls) = pe.tls_directory(&data)? {
                let index = teb::alloc_tls_index();
                *(image.add(tls.address_of_index as usize) as *mut u32) = index;
                let raw_len = tls.end_address_of_raw_data.saturating_sub(tls.start_address_of_raw_data) as usize;
                if tls.start_address_of_raw_data as usize + raw_len > size {
                    bail!("template TLS fuori dall'immagine");
                }
                // Il template si copia dall'immagine già rilocata, non dal file: con una base
                // diversa dalla preferita i puntatori che contiene sono già corretti.
                let raw = std::slice::from_raw_parts(image.add(tls.start_address_of_raw_data as usize), raw_len).to_vec();
                teb::register_tls_module(teb::TlsTemplate { index, raw, zero_fill: tls.size_of_zero_fill });
                teb::ensure_blocks_for_current_thread();
                if !tls.callbacks.is_empty() {
                    // DAP 7.3.7 non ne ha; se uno arrivasse, fallire esplicitamente.
                    bail!("callback TLS non supportati ({})", tls.callbacks.len());
                }
            }

            // Protezioni per sezione come le imporrebbe Windows (il loader scrive prima).
            for s in &pe.sections {
                if s.virtual_address == 0 {
                    continue;
                }
                // PAGE_* equivalente, che VirtualProtect restituirà come protezione precedente.
                let page = match (s.is_executable(), s.is_writable()) {
                    (true, true) => 0x40,  // PAGE_EXECUTE_READWRITE
                    (true, false) => 0x20, // PAGE_EXECUTE_READ
                    (false, true) => 0x04, // PAGE_READWRITE
                    (false, false) => 0x02, // PAGE_READONLY
                };
                let len = (s.virtual_size as usize).next_multiple_of(0x1000);
                let addr = base + s.virtual_address as usize;
                if libc::mprotect(addr as *mut c_void, len, shims::mm::prot_of(page)) != 0 {
                    bail!("mprotect della sezione a rva {:#x} fallita", s.virtual_address);
                }
                shims::mm::record_protection(addr, len, page);
            }

            // DllMain.
            // L'immagine va registrata prima di DllMain: DisableThreadLibraryCalls e
            // GetModuleFileNameW (chiamate da lì) la cercano per base.
        }

        let exports_map = pe.exports(&data)?;
        let mut exports = HashMap::with_capacity(exports_map.len());
        for (name, rva) in exports_map {
            exports.insert(name, base + rva as usize);
        }
        let resource_off = pe
            .rva_to_offset(&data, pe.directories[pe::IMAGE_DIRECTORY_ENTRY_RESOURCE].virtual_address)
            .unwrap_or(0);
        let entry = base + pe.address_of_entry_point as usize;

        let image = Arc::new(Image {
            base,
            size,
            path: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
            exports,
            resource_file: data,
            resource_off,
            entry,
            thread_calls: AtomicBool::new(true),
            _mapping: Mapping(base, size),
        });
        images().lock().unwrap().push(image.clone());

        let r = unsafe { image.dll_main(1 /* DLL_PROCESS_ATTACH */) };
        if r == 0 {
            images().lock().unwrap().retain(|i| !Arc::ptr_eq(i, &image));
            bail!("DllMain(DLL_PROCESS_ATTACH) ha restituito FALSE");
        }
        Ok(image)
    }

    unsafe fn dll_main(&self, reason: u32) -> u32 {
        let dll_main: unsafe extern "win64" fn(*mut c_void, u32, *mut c_void) -> u32 = unsafe { std::mem::transmute(self.entry) };
        unsafe { dll_main(self.base as *mut c_void, reason, std::ptr::null_mut()) }
    }

    /// DisableThreadLibraryCalls.
    pub fn disable_thread_calls(&self) {
        self.thread_calls.store(false, Ordering::Relaxed);
    }

    pub fn thread_calls_enabled(&self) -> bool {
        self.thread_calls.load(Ordering::Relaxed)
    }


    /// Indirizzo di una funzione esportata.
    pub fn export(&self, name: &str) -> Option<usize> {
        self.exports.get(name).copied()
    }

    /// Numero di funzioni esportate per nome.
    pub fn export_count(&self) -> usize {
        self.exports.len()
    }

    pub fn find_export_containing(addr: usize) -> Option<Arc<Image>> {
        images().lock().unwrap().iter().find(|i| (addr >= i.base) && addr < i.base + i.size).cloned()
    }

    /// Cerca una risorsa per id tipo/id nome: (puntatore dati, dimensione).
    pub fn find_resource(&self, type_id: u32, name_id: u32) -> Option<(*const u8, usize)> {
        let (rva, size) = pe::find_resource(&self.resource_file, self.resource_off, type_id, name_id)?;
        Some(((self.base + rva as usize) as *const u8, size as usize))
    }
}

/// Indirizzo → immagine che lo contiene (per GetModuleHandleExW, RtlPcToFileHeader, ...).
pub fn image_containing(addr: usize) -> Option<Arc<Image>> {
    images().lock().unwrap().iter().find(|i| addr >= i.base && addr < i.base + i.size).cloned()
}

/// DLL_THREAD_ATTACH/DETACH ai moduli che non hanno chiamato DisableThreadLibraryCalls, dal
/// thread che nasce o finisce (CreateThread e il thread di lavoro RTWorkQ; i thread non nostri,
/// come quello di PipeWire, non le ricevono: DAP 7.3.7 le disattiva comunque). Con `-v` si
/// stampa quando succede, così un modulo che le vuole non passa inosservato.
pub fn notify_thread(reason: u32) {
    let targets: Vec<Arc<Image>> = images().lock().unwrap().iter().filter(|i| i.thread_calls_enabled()).cloned().collect();
    for image in targets {
        if crate::shims::etw::VERBOSE.load(Ordering::Relaxed) {
            eprintln!("[os-pe-loader] DllMain({}) per {}", if reason == DLL_THREAD_ATTACH { "THREAD_ATTACH" } else { "THREAD_DETACH" }, image.path.display());
        }
        unsafe { image.dll_main(reason) };
    }
}

/// Immagine per HMODULE (= indirizzo base).
pub fn image_by_base(base: usize) -> Option<Arc<Image>> {
    images().lock().unwrap().iter().find(|i| i.base == base).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_headers() {
        let path = std::path::PathBuf::from(
            std::env::var("OS_DAP_DLL").unwrap_or_else(|_| "/home/user/Documenti/dolby-bins/DolbyAudioProcessing.dll".into()),
        );
        let Ok(data) = std::fs::read(&path) else {
            eprintln!("DLL di test assente: salto parse_headers");
            return;
        };
        let pe = Pe::parse(&data).unwrap();
        assert_eq!(pe.sections.len(), 9);
        assert_eq!(pe.size_of_image, 0x435000);
        let tls = pe.tls_directory(&data).unwrap().expect("TLS");
        assert_eq!(tls.end_address_of_raw_data - tls.start_address_of_raw_data, 8);
        assert!(tls.callbacks.is_empty());
        let imports = pe.import_descriptors(&data).unwrap();
        assert_eq!(imports.len(), 31);
        assert!(imports.iter().any(|d| d.dll.contains("RTWorkQ")));
        let exports = pe.exports(&data).unwrap();
        assert!(exports.contains_key("DllGetActivationFactory"));
        assert!(exports.contains_key("DllCanUnloadNow"));
        // La risorsa del tuning (tipo 255, nome 254) deve risultare con i byte giusti.
        let res_off = pe.rva_to_offset(&data, pe.directories[pe::IMAGE_DIRECTORY_ENTRY_RESOURCE].virtual_address).unwrap();
        let (rva, size) = pe::find_resource(&data, res_off, 255, 254).unwrap();
        assert_eq!(size, 17260);
        let off = pe.rva_to_offset(&data, rva).unwrap();
        assert_eq!(&data[off..off + 2], b"{\"");
        // I default CFG del modulo sono stub no-op validi (ret / jmp rax).
        let check = 0x153ab0;
        let disp_off = pe.rva_to_offset(&data, check).unwrap();
        assert_eq!(&data[disp_off..disp_off + 3], &[0xc2, 0x00, 0x00]);
        let dispatch = 0x18d190;
        let doff = pe.rva_to_offset(&data, dispatch).unwrap();
        assert_eq!(&data[doff..doff + 2], &[0xff, 0xe0]);
    }
}
