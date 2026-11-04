//! Nodo PipeWire di open-surround (fase 4, doc sez. 12.13): due sink virtuali davanti al modulo
//! Dolby originale e un'uscita stereo binaurale, con la stessa catena di Windows.
//!
//! Uso:
//!   os-pipewire [opzioni] --capture <dir> --dll <DolbyAudioProcessing.dll>
//!   Sink creati (il prefisso cambia con --name):
//!     open-surround-7.1      giochi: bed 7.1 (7.1.4 con --channels 12), −3 dB prima del modulo
//!     open-surround-stereo   applicazioni normali: oggetti bypass 17-18, 0 dB
//!     open-surround-output   uscita binaurale verso il dispositivo (o --target)
//!   Il volume dei due sink (uno solo: cambiarne uno cambia l'altro) è applicato dopo il
//!   modulo, come il volume dell'endpoint di Windows; prima del modulo il segnale non cambia.
//!
//!   Opzioni del modulo, come os-dap-linux:
//!     --capture <dir>    cartella di cattura con i blob Dolby; ripetibile
//!     --endpoint <hex8>  endpoint da usare nelle catture (default: il primo con ,5)
//!     --mode <m>         cuffie (default) oppure speaker
//!     --init <file>      sostituisce il blob init-time della modalità scelta
//!     --runtime <file>   sostituisce il blob runtime della modalità scelta
//!     --dll <file>       DolbyAudioProcessing.dll
//!     --blob-skip <n>    byte di header del valore di registro da scartare (default 8)
//!     --hp-render <n>    modalità di rendering cuffie OAMD dei bed, 0-3 (default 1)
//!     --block <n>        frame per chiamata al modulo, multiplo di 256 (default 256)
//!     -v                 log del modulo e statistiche del nodo ogni secondo
//!   Opzioni del nodo:
//!     --channels <n>     canali del sink dei giochi: 8 (7.1, default) o 12 (7.1.4)
//!     --gain-db <x>      guadagno dei giochi prima del modulo (default −3, come Windows)
//!     --volume-db <x>    guadagno fisso dopo il modulo, oltre al volume dei sink (default 0)
//!     --name <prefisso>  prefisso dei nomi dei nodi (default open-surround)
//!     --target <nodo>    nodo su cui suonare l'uscita (default: scelta del session manager)
//!     --latency <n>      quantum richiesto al grafo, in frame a 48 kHz (node.latency)
//!     --idle-ms <n>      dopo n ms senza flussi (la coda del modulo è finita) il modulo viene
//!                        riconfigurato e fermato fino al flusso successivo, che riparte da uno
//!                        stato pulito come su Windows (default 2000; 0 = mai)
//!     --always-on        uscita non passiva: il dispositivo resta attivo anche senza flussi

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod node;

use anyhow::{bail, Context, Result};
use os_dap_core::options::ModuleOptions;

pub struct Args {
    pub module: ModuleOptions,
    pub channels: usize,
    pub gain_db: Option<f32>,
    pub volume_db: f32,
    pub name: String,
    pub target: Option<String>,
    pub latency: Option<u32>,
    pub idle_ms: u32,
    pub always_on: bool,
}

fn usage() -> String {
    include_str!("main.rs")
        .lines()
        .take_while(|l| l.starts_with("//!"))
        .map(|l| l.trim_start_matches("//!"))
        .collect::<Vec<_>>()
        .join("\n")
}

impl Args {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut a = Args {
            module: ModuleOptions::default(),
            channels: 8,
            gain_db: None,
            volume_db: 0.0,
            name: "open-surround".into(),
            target: None,
            latency: None,
            idle_ms: 2000,
            always_on: false,
        };
        let mut it = args.into_iter();
        while let Some(flag) = it.next() {
            let mut val = || it.next().with_context(|| format!("manca il valore di {flag}"));
            if a.module.parse_flag(&flag, &mut val)? {
                continue;
            }
            match flag.as_str() {
                "--channels" => {
                    a.channels = val()?.parse()?;
                    if a.channels != 8 && a.channels != 12 {
                        bail!("--channels: 8 (7.1) o 12 (7.1.4)");
                    }
                }
                "--gain-db" => a.gain_db = Some(val()?.parse()?),
                "--volume-db" => a.volume_db = val()?.parse()?,
                "--name" => a.name = val()?,
                "--target" => a.target = Some(val()?),
                "--latency" => a.latency = Some(val()?.parse()?),
                "--idle-ms" => a.idle_ms = val()?.parse()?,
                "--always-on" => a.always_on = true,
                "-h" | "--help" => {
                    eprintln!("{}", usage());
                    std::process::exit(0);
                }
                _ => bail!("opzione sconosciuta: {flag} (vedi --help)"),
            }
        }
        if a.module.blobs.captures.is_empty() && (a.module.blobs.init.is_none() || a.module.blobs.runtime.is_none()) {
            bail!("servono i blob Dolby: --capture <dir> (vedi --help)");
        }
        Ok(a)
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn main() -> Result<()> {
    node::run(Args::parse(std::env::args().skip(1))?)
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn main() {
    let _ = Args::parse(std::env::args().skip(1));
    eprintln!("os-pipewire è il nodo PipeWire per Linux x86-64");
    std::process::exit(1);
}
