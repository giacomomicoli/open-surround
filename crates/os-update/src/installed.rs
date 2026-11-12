//! `~/.local/share/open-surround/installed.json`: che cosa è installato e da dove. Lo scrivono
//! `install.sh` (dal repository o dal pacchetto) e l'aggiornamento.
//!
//! ```json
//! {"version": "0.2.0", "commit": "0123…", "source": "release", "dll_sha256": "0fad…"}
//! ```
//!
//! `source` è `release` (pacchetto di una release: aggiornabile dalla finestra) o `repo`
//! (compilato dal repository: solo avviso).

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub version: String,
    #[serde(default)]
    pub commit: String,
    pub source: String,
    #[serde(default)]
    pub dll_sha256: Option<String>,
}

impl Installed {
    pub fn is_release(&self) -> bool {
        self.source == "release"
    }

    /// `None` se il file non c'è (installazione precedente a questo file, o nessuna).
    pub fn load(path: &Path) -> Result<Option<Installed>> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(Some(serde_json::from_str(&s).with_context(|| format!("{}: illeggibile", path.display()))?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| path.display().to_string()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut s = serde_json::to_string_pretty(self)?;
        s.push('\n');
        crate::paths::write_atomic(path, s.as_bytes(), 0o644)
    }
}

/// `versione.json` del pacchetto (lo scrive `package.sh`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PackageVersion {
    pub version: String,
    #[serde(default)]
    pub commit: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn andata_e_ritorno() {
        let p = std::env::temp_dir().join(format!("os-update-installed-{}/installed.json", std::process::id()));
        assert_eq!(Installed::load(&p).unwrap(), None);
        let i = Installed { version: "0.2.0".into(), commit: "abc".into(), source: "release".into(), dll_sha256: Some("00".into()) };
        i.save(&p).unwrap();
        assert_eq!(Installed::load(&p).unwrap(), Some(i));
        // come lo scrive install.sh (senza DLL: null)
        std::fs::write(&p, r#"{"version":"0.1.0","commit":"x","source":"repo","dll_sha256":null}"#).unwrap();
        assert!(!Installed::load(&p).unwrap().unwrap().is_release());
        std::fs::write(&p, "{").unwrap();
        assert!(Installed::load(&p).is_err());
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
}
