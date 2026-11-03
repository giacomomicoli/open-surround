//! os-dap-linux: backend Linux di open-surround (fase 3). Il loader PE nativo di
//! `os-pe-loader` carica `DolbyAudioProcessing.dll` originale; tutto il resto (blob, oggetti,
//! catena di guadagno, elaborazione WAV) è nel nucleo comune `os-dap-core`, identico a
//! Windows. Opzioni: `os-dap-linux --help` (vedi os_dap_core::offline).

// Le vtable COM elencano funzioni come indirizzi: è il meccanismo stesso di una vtable.
#![allow(function_casts_as_integer)]

mod backend;
mod com;

fn main() -> anyhow::Result<()> {
    let args = os_dap_core::offline::Args::from_env()?;
    os_dap_core::offline::run::<backend::LinuxBackend>(&args)
}
