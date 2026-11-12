//! Preferenze del controllo automatico, in `~/.config/open-surround/aggiornamenti.json`: se
//! controllare all'avvio della finestra e quando è stato fatto l'ultimo controllo (al più uno
//! al giorno).

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub const DAY: u64 = 24 * 3600;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefs {
    /// Controllo automatico all'avvio della finestra.
    #[serde(default = "yes")]
    pub automatico: bool,
    /// Secondi Unix dell'ultimo controllo riuscito.
    #[serde(default)]
    pub ultimo_controllo: u64,
}

fn yes() -> bool {
    true
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { automatico: true, ultimo_controllo: 0 }
    }
}

impl Prefs {
    /// Default se il file manca o non si legge: le preferenze non devono impedire di aprire
    /// la finestra.
    pub fn load(path: &Path) -> Prefs {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::paths::write_atomic(path, serde_json::to_string_pretty(self)?.as_bytes(), 0o644)
    }

    /// Il controllo automatico va fatto ora (`now` in secondi Unix).
    pub fn due(&self, now: u64) -> bool {
        self.automatico && (now < self.ultimo_controllo || now - self.ultimo_controllo >= DAY)
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn una_volta_al_giorno() {
        let mut p = Prefs::default();
        assert!(p.due(1_000_000));
        p.ultimo_controllo = 1_000_000;
        assert!(!p.due(1_000_000 + DAY - 1));
        assert!(p.due(1_000_000 + DAY));
        assert!(p.due(10), "orologio tornato indietro");
        p.automatico = false;
        assert!(!p.due(1_000_000 + 5 * DAY));
        let f = std::env::temp_dir().join(format!("os-update-prefs-{}/a.json", std::process::id()));
        assert_eq!(Prefs::load(&f), Prefs::default());
        p.save(&f).unwrap();
        assert_eq!(Prefs::load(&f), p);
        std::fs::write(&f, "{}").unwrap();
        assert_eq!(Prefs::load(&f), Prefs::default());
        std::fs::remove_dir_all(f.parent().unwrap()).unwrap();
    }
}
