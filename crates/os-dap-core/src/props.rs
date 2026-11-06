//! Proprietà che il modulo legge dall'endpoint: i blob Dolby del contesto FX (catturati da
//! Windows con capture_asar.ps1 o ref-capture) e le proprietà dell'endpoint stesso.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::layout::Mode;

/// GUID come intero a 128 bit, nella stessa notazione di `windows_core::GUID::from_u128`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Guid(pub u128);

impl Guid {
    pub fn data1(self) -> u32 {
        (self.0 >> 96) as u32
    }
}

/// Stessa forma del Debug di `windows_core::GUID` (maiuscolo, senza graffe): il modulo riceve
/// questa stringa come PKEY_AudioEndpoint_GUID e la usa per nominare la memoria condivisa.
impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let v = self.0;
        write!(
            f,
            "{:08X}-{:04X}-{:04X}-{:04X}-{:012X}",
            (v >> 96) as u32,
            (v >> 80) as u16,
            (v >> 64) as u16,
            (v >> 48) as u16,
            v & 0xffff_ffff_ffff
        )
    }
}

/// PROPERTYKEY.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct PropKey {
    pub fmtid: Guid,
    pub pid: u32,
}

impl fmt::Display for PropKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{{{:?}}},{}", self.fmtid, self.pid)
    }
}

/// Valore di una proprietà: VT_BLOB o VT_LPWSTR.
#[derive(Clone, Debug)]
pub enum Value {
    Blob(Vec<u8>),
    Str(String),
}

/// Set di proprietà DAP-VR "legacy" nel contesto Dolby dell'endpoint (pid per modalità in `Mode::pids`).
pub const PKEY_DAPVR_FMTID: Guid = Guid(0x1b4dab55_b1fb_4d8c_8317_f2d4a96efbb8);
/// Altri fmtid presenti nel contesto, salvati da capture_asar.ps1 come <ep>-<hex8>-<pid>.bin.
pub const OTHER_FMTIDS: &[Guid] = &[
    Guid(0xe36464a1_2f4b_440b_a776_8b32b26a7f01),
    Guid(0xd5e60007_c479_4002_bebe_075a1613a70d),
];
/// Chiavi notificate da Windows al modulo a ogni scrittura di Dolby Access, nell'ordine della
/// traccia ETW (doc sez. 12.13, W8): PKEY_DAPVR_INITTIME_DABS, PKEY_DAPVR_RUNTIME_DABS,
/// PKEY_DAPVR_INITTIME_DAHP, PKEY_DAPVR_RUNTIME_DAHP, DAFM_PROFILE.
pub const PROFILE_CHANGE_KEYS: [PropKey; 5] = [
    PropKey { fmtid: PKEY_DAPVR_FMTID, pid: 4 },
    PropKey { fmtid: PKEY_DAPVR_FMTID, pid: 5 },
    PropKey { fmtid: PKEY_DAPVR_FMTID, pid: 1 },
    PropKey { fmtid: PKEY_DAPVR_FMTID, pid: 2 },
    PropKey { fmtid: OTHER_FMTIDS[0], pid: 1 },
];
/// PKEY_AudioEndpoint_GUID (mmdeviceapi.h).
pub const PKEY_AUDIOENDPOINT_GUID: PropKey = PropKey { fmtid: Guid(0x1da5d803_d492_4edd_8c23_e0c0ffee7f0e), pid: 4 };
/// PKEY_Device_FriendlyName / DEVPKEY_Device_FriendlyName.
pub const PKEY_DEVICE_FRIENDLYNAME: PropKey = PropKey { fmtid: Guid(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };

/// GUID arbitrario dell'endpoint virtuale (il modulo lo usa solo per nominare la memoria condivisa).
pub const ENDPOINT_GUID: Guid = Guid(0x6f70656e_7375_7272_6f75_6e6400000001);
pub const ENDPOINT_FRIENDLY_NAME: &str = "open-surround (cuffie virtuali)";

/// Proprietà dell'endpoint virtuale (IMMDevice::OpenPropertyStore e pAPOEndpointProperties).
pub fn endpoint_properties(endpoint: Guid) -> Vec<(PropKey, Value)> {
    vec![
        (PKEY_AUDIOENDPOINT_GUID, Value::Str(format!("{{{endpoint:?}}}"))),
        (PKEY_DEVICE_FRIENDLYNAME, Value::Str(ENDPOINT_FRIENDLY_NAME.into())),
    ]
}

/// Id dell'IMMDevice virtuale, nella forma degli endpoint di render di Windows.
pub fn device_id(endpoint: Guid) -> String {
    format!("{{0.0.0.00000000}}.{{{endpoint:?}}}")
}

/// Da dove vengono i blob Dolby del contesto FX.
#[derive(Clone, Debug, Default)]
pub struct BlobSource {
    /// Cartelle di cattura; le successive sovrascrivono i valori delle precedenti.
    pub captures: Vec<PathBuf>,
    /// Endpoint (8 cifre esadecimali) da usare nelle catture; se assente il primo con `,5`.
    pub endpoint: Option<String>,
    /// Sostituiscono i blob init-time e runtime della modalità scelta.
    pub init: Option<PathBuf>,
    pub runtime: Option<PathBuf>,
    /// Byte di header del valore di registro da scartare.
    pub blob_skip: usize,
}

/// Interpreta il nome di un file di cattura: `<ep>-<pid>.bin` o `<ep>-<hex8>-<pid>.bin`.
pub fn parse_capture_name(name: &str) -> Option<(String, PropKey)> {
    let stem = name.strip_suffix(".bin")?;
    let parts: Vec<&str> = stem.split('-').collect();
    let (ep, fmtid, pid) = match parts.as_slice() {
        [ep, pid] => (ep, PKEY_DAPVR_FMTID, pid),
        [ep, hex, pid] => {
            let fmtid = *OTHER_FMTIDS.iter().find(|g| format!("{:08x}", g.data1()) == *hex)?;
            (ep, fmtid, pid)
        }
        _ => return None,
    };
    Some((ep.to_lowercase(), PropKey { fmtid, pid: pid.parse().ok()? }))
}

/// Raccoglie i file del contesto Dolby per un endpoint dalle cartelle di cattura.
fn load_captures(dirs: &[PathBuf], endpoint: &mut Option<String>) -> Result<BTreeMap<PropKey, PathBuf>> {
    let mut values = BTreeMap::new();
    for dir in dirs.iter().rev() {
        let mut files: Vec<(String, PropKey, PathBuf)> = std::fs::read_dir(dir)
            .with_context(|| format!("cartella di cattura {}", dir.display()))?
            .filter_map(|e| {
                let e = e.ok()?;
                let (ep, key) = parse_capture_name(e.file_name().to_str()?)?;
                Some((ep, key, e.path()))
            })
            .collect();
        files.sort_by(|a, b| a.2.cmp(&b.2));
        if endpoint.is_none() {
            *endpoint = files.iter().find(|f| f.1.fmtid == PKEY_DAPVR_FMTID && f.1.pid == 5).map(|f| f.0.clone());
        }
        for (ep, key, path) in files {
            if Some(&ep) == endpoint.as_ref() {
                // Le cartelle sono visitate dall'ultima: la prima occorrenza vince.
                values.entry(key).or_insert(path);
            }
        }
    }
    Ok(values)
}

fn read_blob(path: &Path, skip: usize) -> Result<Vec<u8>> {
    let data = std::fs::read(path).with_context(|| format!("lettura {}", path.display()))?;
    if data.len() <= skip {
        bail!("{} troppo corto", path.display());
    }
    Ok(data[skip..].to_vec())
}

impl BlobSource {
    /// Gli stessi blob letti da un'altra cartella di cattura (stesso endpoint, stesso header):
    /// il profilo d'arrivo di un cambio a caldo. Tutti i valori della cartella, come Dolby
    /// Access che riscrive il contesto intero.
    pub fn with_capture(&self, dir: PathBuf) -> BlobSource {
        BlobSource { captures: vec![dir], init: None, runtime: None, ..self.clone() }
    }

    /// Valori del property store FX "user" (IAudioSystemEffectsPropertyStore::OpenUserPropertyStore),
    /// ordinati per chiave. Verifica che ci siano i blob init e runtime della modalità.
    pub fn fx_values(&self, mode: Mode) -> Result<Vec<(PropKey, Value)>> {
        let mut endpoint = self.endpoint.clone();
        let mut files = load_captures(&self.captures, &mut endpoint)?;
        let (pid_init, pid_runtime) = mode.pids();
        for (pid, path) in [(pid_init, &self.init), (pid_runtime, &self.runtime)] {
            if let Some(path) = path {
                files.insert(PropKey { fmtid: PKEY_DAPVR_FMTID, pid }, path.clone());
            }
        }
        for pid in [pid_init, pid_runtime] {
            if !files.contains_key(&PropKey { fmtid: PKEY_DAPVR_FMTID, pid }) {
                bail!("manca il valore {{1b4dab55-…}},{pid} ({mode:?}): aggiungere una cattura completa o --init/--runtime");
            }
        }
        eprintln!("endpoint delle catture: {}", endpoint.as_deref().unwrap_or("-"));
        let mut values = Vec::new();
        for (key, path) in &files {
            eprintln!("  {key} <- {}", path.display());
            values.push((*key, Value::Blob(read_blob(path, self.blob_skip)?)));
        }
        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_format() {
        assert_eq!(format!("{:?}", PKEY_DAPVR_FMTID), "1B4DAB55-B1FB-4D8C-8317-F2D4A96EFBB8");
        assert_eq!(format!("{}", PKEY_AUDIOENDPOINT_GUID), "{1DA5D803-D492-4EDD-8C23-E0C0FFEE7F0E},4");
    }

    #[test]
    fn capture_names() {
        let (ep, k) = parse_capture_name("32ED16B6-2.bin").unwrap();
        assert_eq!((ep.as_str(), k), ("32ed16b6", PropKey { fmtid: PKEY_DAPVR_FMTID, pid: 2 }));
        let (_, k) = parse_capture_name("32ed16b6-d5e60007-2.bin").unwrap();
        assert_eq!(k.fmtid, OTHER_FMTIDS[1]);
        assert!(parse_capture_name("32ed16b6-deadbeef-2.bin").is_none());
        assert!(parse_capture_name("info.txt").is_none());
    }
}
