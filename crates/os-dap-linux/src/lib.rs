//! os-dap-linux: backend Linux di open-surround (fase 3). Il loader PE nativo di
//! `os-pe-loader` carica `DolbyAudioProcessing.dll` originale e [`LinuxBackend`] lo pilota
//! con la stessa sequenza COM del backend Windows. Lo usano l'elaborazione offline
//! (`os-dap-linux`, questo crate) e il nodo PipeWire (`os-pipewire`, fase 4).

// Le vtable COM elencano funzioni come indirizzi: è il meccanismo stesso di una vtable.
#![allow(function_casts_as_integer)]

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod backend;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod com;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub use backend::LinuxBackend;
