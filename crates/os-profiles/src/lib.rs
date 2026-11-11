//! os-profiles: i profili di Dolby Access (fase 5 della roadmap, doc sez. 12.14).
//!
//! Il codec dei sei valori del contesto Dolby dell'endpoint (L1), con lettura e
//! scrittura identiche byte per byte a quello che scrive Dolby Access. Il modello dei profili
//! (L2, [`Profile`]) produce un [`DolbyContext`] identico per i 19 stati catturati.
//! [`Settings`] è la logica dell'interfaccia della fase 6 (doc 12.15): il profilo attivo e le
//! impostazioni di ognuno dei sette profili, con il loro file JSON.

pub mod blob;
pub mod check;
pub mod model;
pub mod settings;
pub mod wire;

pub use blob::{DafmProfile, DapVrInit, DapVrRuntime, Decoded, DolbyContext, D5e60007};
pub use model::{resolve, Custom, Ieq, Profile};
pub use settings::{Kind, Settings};
