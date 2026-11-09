//! os-dap-linux: backend Linux di open-surround (fase 3). Il loader PE nativo di
//! `os-pe-loader` carica `DolbyAudioProcessing.dll` originale; tutto il resto (blob, oggetti,
//! catena di guadagno, elaborazione WAV) è nel nucleo comune `os-dap-core`, identico a
//! Windows. Opzioni: `os-dap-linux --help` (vedi os_dap_core::offline).

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn main() -> anyhow::Result<()> {
    let mut args = os_dap_core::offline::Args::from_env()?;
    args.module.blobs.resolver = Some(os_profiles::resolve);
    os_dap_core::offline::run::<os_dap_linux::LinuxBackend>(&args)
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn main() {
    eprintln!("os-dap-linux è il backend Linux x86-64: su Windows usare os-dap-host");
    std::process::exit(1);
}
