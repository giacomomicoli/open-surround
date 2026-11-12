//! os-update: controllo degli aggiornamenti e aggiornamento sul posto di un'installazione di
//! open-surround da una release firmata su GitHub (doc 12.15, "Aggiornamenti").
//!
//! - [`semver`]: confronto fra versioni.
//! - [`sign`]: firma minisign (Ed25519) del pacchetto con la chiave pubblica compilata dentro.
//! - [`archive`]: estrazione del tar.gz con solo le voci attese.
//! - [`dll`]: la copia installata di `DolbyAudioProcessing.dll` (verificata, sconosciuta, assente).
//! - [`installed`]: `~/.local/share/open-surround/installed.json`, scritto da `install.sh`.
//! - [`github`]: l'ultima release dall'API di GitHub.
//! - [`apply`]: i nove passi dell'aggiornamento, con ritorno alla versione precedente.
//! - [`prefs`]: controllo automatico (sì/no, ultimo controllo) per la finestra.
//!
//! Serve solo su Linux (installazione in `~/.local`, systemd); compila anche altrove perché
//! il workspace si compila anche su Windows (W1).

pub mod apply;
pub mod archive;
pub mod dll;
pub mod github;
pub mod installed;
pub mod paths;
pub mod prefs;
pub mod semver;
pub mod sign;

pub use apply::{apply, check, ApplyOptions, CheckReport, Config, UpdateError};
