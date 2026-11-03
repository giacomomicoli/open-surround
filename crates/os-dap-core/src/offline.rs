//! Elaborazione offline di un WAV multicanale con il modulo Dolby DAP-VR originale.
//! Stessa riga di comando su ogni piattaforma; cambia solo il backend che carica il modulo.
//!
//! Uso:
//!   os-dap-host [opzioni] --capture <dir> <ingresso.wav> <uscita.wav>
//!     --capture <dir>    cartella di cattura (docs/reference/captures/<nome>, vedi
//!                        capture_asar.ps1); ripetibile: le cartelle successive
//!                        sovrascrivono i valori delle precedenti
//!     --endpoint <hex8>  endpoint da usare nelle catture (default: il primo con ,5)
//!     --mode <m>         cuffie (default, DAHP: legge {1b4dab55-…},1 + ,2) oppure
//!                        speaker (DABS, speaker integrati: legge ,4 + ,5)
//!     --init <file>      sostituisce il blob init-time della modalità scelta
//!     --runtime <file>   sostituisce il blob runtime della modalità scelta
//!     --dll <file>       DolbyAudioProcessing.dll (default C:\tools\dolby-bins\…)
//!     --blob-skip <n>    byte di header del valore di registro da scartare (default 8)
//!     --stereo-bypass    un ingresso stereo va negli oggetti bypass 17-18, come lo stereo di
//!                        un'applicazione normale su Windows (default: bed FL/FR, come un gioco)
//!     --hp-render <n>    modalità di rendering cuffie OAMD dei bed, 0-3 (default 1, come
//!                        Windows: con 0 il modulo non virtualizza)
//!     --gain-db <x>      guadagno sull'ingresso prima del modulo (default come Windows: -3,
//!                        0 con --stereo-bypass)
//!     --volume-db <x>    volume sull'uscita del modulo (default 0). Windows qui applica il
//!                        volume dell'endpoint: catena verificata -3 dB → Dolby → volume
//!     --block <n>        frame per blocco, multiplo di 256 (default 256)
//!     --tail-ms <n>      silenzio aggiunto in coda per svuotare i filtri (default 500)
//!     -v                 traccia le chiamate del modulo agli oggetti dell'host
//!                        e mostra il log interno del modulo

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::engine::{Backend, Engine, ModuleSetup};
use crate::layout::{db_to_gain, Mode, Routing, OUT_CHANNELS, SAMPLE_RATE};
use crate::props::BlobSource;

#[cfg(windows)]
const DEFAULT_DLL: &str = r"C:\tools\dolby-bins\DolbyAudioProcessing.dll";
#[cfg(not(windows))]
const DEFAULT_DLL: &str = "DolbyAudioProcessing.dll";

pub struct Args {
    pub input: PathBuf,
    pub output: PathBuf,
    pub dll: PathBuf,
    pub blobs: BlobSource,
    pub mode: Mode,
    pub block: usize,
    pub tail_ms: u32,
    pub hp_render: u8,
    pub stereo_bypass: bool,
    pub gain_db: Option<f32>,
    pub volume_db: f32,
    pub verbose: bool,
}

pub fn usage() -> String {
    include_str!("offline.rs")
        .lines()
        .take_while(|l| l.starts_with("//!"))
        .map(|l| l.trim_start_matches("//!"))
        .collect::<Vec<_>>()
        .join("\n")
}

impl Args {
    pub fn from_env() -> Result<Self> {
        Self::parse(std::env::args().skip(1))
    }

    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut positional = Vec::new();
        let mut dll = PathBuf::from(DEFAULT_DLL);
        let mut blobs = BlobSource { blob_skip: 8, ..Default::default() };
        let (mut block, mut tail_ms) = (256, 500);
        let mut mode = Mode::Headphones;
        let mut hp_render = 1u8;
        let mut stereo_bypass = false;
        let mut gain_db = None;
        let mut volume_db = 0.0f32;
        let mut verbose = false;
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            let mut val = || it.next().with_context(|| format!("manca il valore di {a}"));
            match a.as_str() {
                "--dll" => dll = val()?.into(),
                "--capture" => blobs.captures.push(PathBuf::from(val()?)),
                "--endpoint" => blobs.endpoint = Some(val()?.to_lowercase()),
                "--mode" => {
                    mode = match val()?.as_str() {
                        "cuffie" => Mode::Headphones,
                        "speaker" => Mode::Speakers,
                        m => bail!("modalità sconosciuta: {m} (cuffie|speaker)"),
                    }
                }
                "--init" => blobs.init = Some(PathBuf::from(val()?)),
                "--runtime" => blobs.runtime = Some(PathBuf::from(val()?)),
                "--blob-skip" => blobs.blob_skip = val()?.parse()?,
                "--block" => block = val()?.parse()?,
                "--hp-render" => hp_render = val()?.parse()?,
                "--stereo-bypass" => stereo_bypass = true,
                "--tail-ms" => tail_ms = val()?.parse()?,
                "--gain-db" => gain_db = Some(val()?.parse()?),
                "--volume-db" => volume_db = val()?.parse()?,
                "-v" | "--verbose" => verbose = true,
                "-h" | "--help" => {
                    eprintln!("{}", usage());
                    std::process::exit(0);
                }
                _ if a.starts_with('-') => bail!("opzione sconosciuta: {a}"),
                _ => positional.push(PathBuf::from(a)),
            }
        }
        let [input, output] = <[PathBuf; 2]>::try_from(positional)
            .map_err(|_| anyhow::anyhow!("servono <ingresso.wav> e <uscita.wav> (vedi --help)"))?;
        Ok(Args { input, output, dll, blobs, mode, block, tail_ms, hp_render, stereo_bypass, gain_db, volume_db, verbose })
    }
}

pub fn read_wav(path: &Path) -> Result<(hound::WavSpec, Vec<f32>)> {
    let mut r = hound::WavReader::open(path).with_context(|| format!("apertura {}", path.display()))?;
    let spec = r.spec();
    let samples = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<Vec<_>, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<Vec<_>, _>>()?
        }
    };
    Ok((spec, samples))
}

/// Elabora `args.input` → `args.output` (stereo float32 a 48 kHz) con il backend `B`.
pub fn run<B: Backend>(args: &Args) -> Result<()> {
    let (spec, mut input) = read_wav(&args.input)?;
    if spec.sample_rate != SAMPLE_RATE {
        bail!(
            "il modulo lavora a 48 kHz, l'ingresso è a {} Hz (convertire prima, es. ffmpeg -ar 48000)",
            spec.sample_rate
        );
    }
    let routing = Routing::new(spec.channels, args.stereo_bypass, args.gain_db)?;
    if routing.gain != 1.0 {
        input.iter_mut().for_each(|s| *s *= routing.gain);
    }
    let in_ch = spec.channels as usize;

    let fx_props = args.blobs.fx_values(args.mode)?;
    let setup = ModuleSetup::new(&args.dll, args.mode, args.block, fx_props, args.verbose)?;
    let mut engine = Engine::<B>::open(&setup)?;
    engine.set_headphone_render_mode(args.hp_render.min(3));
    eprintln!("modulo DAP configurato ({:?}, blocco {} frame, {} canali in ingresso)", args.mode, args.block, in_ch);

    let frames_in = input.len() / in_ch;
    let tail = (args.tail_ms as usize * SAMPLE_RATE as usize) / 1000;
    let total = frames_in + tail;
    let block = engine.block_frames();
    let mut out = Vec::with_capacity(total.div_ceil(block) * block * OUT_CHANNELS);

    let mut pos = 0;
    while pos < total {
        for (c, &b) in routing.map.iter().enumerate() {
            let buf = engine.input_mut(b);
            for (i, s) in buf.iter_mut().enumerate() {
                let f = pos + i;
                *s = if f < frames_in { input[f * in_ch + c] } else { 0.0 };
            }
        }
        out.extend_from_slice(engine.process(&routing.active)?);
        pos += block;
    }

    // Windows applica il volume dell'endpoint dopo il modulo.
    let volume = db_to_gain(args.volume_db);
    if volume != 1.0 {
        out.iter_mut().for_each(|s| *s *= volume);
    }
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let rms = (out.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / out.len().max(1) as f64).sqrt();
    let mut w = hound::WavWriter::create(
        &args.output,
        hound::WavSpec { channels: OUT_CHANNELS as u16, sample_rate: SAMPLE_RATE, bits_per_sample: 32, sample_format: hound::SampleFormat::Float },
    )?;
    for s in &out {
        w.write_sample(*s)?;
    }
    w.finalize()?;
    eprintln!(
        "scritto {} ({} frame, picco {:.1} dBFS, RMS {:.1} dBFS)",
        args.output.display(),
        out.len() / OUT_CHANNELS,
        20.0 * (peak.max(1e-10) as f64).log10(),
        20.0 * rms.max(1e-10).log10()
    );
    Ok(())
}
