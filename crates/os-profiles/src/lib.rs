//! os-profiles: i profili di Dolby Access (fase 5 della roadmap, doc sez. 12.14).
//!
//! Il codec dei sei valori del contesto Dolby dell'endpoint (L1), con lettura e
//! scrittura identiche byte per byte a quello che scrive Dolby Access. Il modello dei profili
//! (L2, [`Profile`]) produce un [`DolbyContext`] identico per i 19 stati catturati.

pub mod blob;
pub mod check;
pub mod model;
pub mod wire;

pub use blob::{DafmProfile, DapVrInit, DapVrRuntime, Decoded, DolbyContext, D5e60007};
pub use model::{Custom, Ieq, Profile};
