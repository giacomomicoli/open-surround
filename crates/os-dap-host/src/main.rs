//! os-dap-host: backend Windows di open-surround. Il sistema carica `DolbyAudioProcessing.dll`
//! originale; tutto il resto (blob, oggetti, catena di guadagno, elaborazione WAV) è nel nucleo
//! comune `os-dap-core`. Opzioni: `os-dap-host --help` (vedi os_dap_core::offline).

mod backend;
mod debuglog;
mod iathook;
mod stubs;

fn main() -> anyhow::Result<()> {
    let args = os_dap_core::offline::Args::from_env()?;
    os_dap_core::offline::run::<backend::WindowsBackend>(&args)
}
