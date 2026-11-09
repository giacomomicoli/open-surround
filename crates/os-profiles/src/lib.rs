//! os-profiles: i profili di Dolby Access (fase 5 della roadmap, doc sez. 12.14).
//!
//! Per ora (passo L1) il codec dei sei valori del contesto Dolby dell'endpoint: lettura e
//! scrittura identiche byte per byte a quello che scrive Dolby Access. Il modello dei profili
//! (L2) produrrà un [`DolbyContext`] da cui il nodo e l'offline prendono i blob.

pub mod blob;
pub mod check;
pub mod wire;

pub use blob::{DafmProfile, DapVrInit, DapVrRuntime, Decoded, DolbyContext, D5e60007};
