//! os-dap-host: backend Windows di open-surround. Il sistema carica `DolbyAudioProcessing.dll`
//! originale; tutto il resto (blob, oggetti, catena di guadagno, elaborazione WAV) è nel nucleo
//! comune `os-dap-core`. Opzioni: `os-dap-host --help` (vedi os_dap_core::offline).

#[cfg(windows)]
mod backend;
#[cfg(windows)]
mod debuglog;
#[cfg(windows)]
mod iathook;
#[cfg(windows)]
mod stubs;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    let mut args = os_dap_core::offline::Args::from_env()?;
    args.module.blobs.resolver = Some(os_profiles::resolve);
    os_dap_core::offline::run::<backend::WindowsBackend>(&args)
}

#[cfg(not(windows))]
fn main() {
    eprintln!("os-dap-host è il backend Windows: su Linux usare os-dap-linux");
    std::process::exit(1);
}
