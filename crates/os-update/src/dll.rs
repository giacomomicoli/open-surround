//! La copia installata del modulo di Dolby, `~/.local/share/open-surround/DolbyAudioProcessing.dll`:
//! è quella che usa il servizio. L'aggiornamento non la tocca, ma la controlla prima di
//! cambiare qualsiasi binario: i binari nuovi sono verificati con le versioni del modulo note.

use std::path::Path;

use anyhow::{bail, Context as _, Result};
use serde::Serialize;

use crate::sign::sha256_hex;

/// Versioni del modulo verificate (doc 12.15, README "Getting the DLL"): SHA-256 → nome.
pub const KNOWN: [(&str, &str); 1] = [("0fada40251880d03c657a357745d362799c93fb6024f733f1c8bf1057f5e4715", "DAP 7.3.7")];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(tag = "stato", rename_all = "kebab-case")]
pub enum DllState {
    /// Versione verificata: si procede.
    Verificato { versione: String, sha256: String },
    /// Presente ma con un hash sconosciuto: si procede solo con una conferma esplicita.
    NonVerificato { sha256: String },
    /// Assente: aggiornamento bloccato prima di toccare i binari.
    Mancante,
}

impl DllState {
    pub fn sha256(&self) -> Option<&str> {
        match self {
            DllState::Verificato { sha256, .. } | DllState::NonVerificato { sha256 } => Some(sha256),
            DllState::Mancante => None,
        }
    }
}

pub fn classify_hash(sha256: &str) -> DllState {
    match KNOWN.iter().find(|(h, _)| *h == sha256) {
        Some((_, name)) => DllState::Verificato { versione: name.to_string(), sha256: sha256.into() },
        None => DllState::NonVerificato { sha256: sha256.into() },
    }
}

/// Lo stato del file `path`.
pub fn check(path: &Path) -> Result<DllState> {
    match std::fs::read(path) {
        Ok(data) => Ok(classify_hash(&sha256_hex(&data))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DllState::Mancante),
        Err(e) => Err(e).with_context(|| path.display().to_string()),
    }
}

/// Testo per chi deve copiare il DLL a mano.
pub fn missing_message(dest: &Path) -> String {
    format!(
        "Manca il modulo di Dolby: {}. Copia lì DolbyAudioProcessing.dll dalla tua installazione di Dolby Access su Windows \
         (C:\\Program Files\\WindowsApps\\DolbyLaboratories.DolbyAccess_…\\…, vedi «Getting the DLL» nel README), \
         oppure sceglilo dalla finestra con «Scegli il DLL…».",
        dest.display()
    )
}

/// Copia il DLL scelto dall'utente al suo posto (in modo atomico) e ne restituisce lo stato.
/// Rifiuta un file che non è un eseguibile PE.
pub fn install_copy(src: &Path, dest: &Path) -> Result<DllState> {
    let data = std::fs::read(src).with_context(|| src.display().to_string())?;
    if data.len() < 0x40 || &data[..2] != b"MZ" {
        bail!("{}: non è un DLL di Windows", src.display());
    }
    crate::paths::write_atomic(dest, &data, 0o644)?;
    Ok(classify_hash(&sha256_hex(&data)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i_tre_casi() {
        let d = std::env::temp_dir().join(format!("os-update-dll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let dll = d.join("DolbyAudioProcessing.dll");
        // assente
        assert_eq!(check(&dll).unwrap(), DllState::Mancante);
        assert!(missing_message(&dll).contains("Scegli il DLL"));
        // presente, sconosciuto
        let mut fake = b"MZ".to_vec();
        fake.resize(0x100, 0);
        std::fs::write(d.join("scelto.dll"), &fake).unwrap();
        let st = install_copy(&d.join("scelto.dll"), &dll).unwrap();
        assert_eq!(st, DllState::NonVerificato { sha256: sha256_hex(&fake) });
        assert_eq!(check(&dll).unwrap(), st);
        // noto (l'hash, il file vero non si può mettere nei test)
        assert_eq!(classify_hash(KNOWN[0].0), DllState::Verificato { versione: "DAP 7.3.7".into(), sha256: KNOWN[0].0.into() });
        // non un PE: rifiutato, la copia installata resta
        std::fs::write(d.join("testo.dll"), b"non un dll").unwrap();
        assert!(install_copy(&d.join("testo.dll"), &dll).is_err());
        assert_eq!(check(&dll).unwrap(), st);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
