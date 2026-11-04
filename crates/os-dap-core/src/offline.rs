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
//!     --quantum <n>      passa dall'adattatore del tempo reale (os_dap_core::realtime) a
//!                        cicli di n frame, come il nodo PipeWire; l'uscita, tolta la latenza
//!                        dell'adattatore, deve essere identica byte per byte a quella senza
//!     --tail-ms <n>      silenzio aggiunto in coda per svuotare i filtri (default 500)
//!     -v                 traccia le chiamate del modulo agli oggetti dell'host
//!                        e mostra il log interno del modulo

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::engine::{Backend, Engine};
use crate::layout::{db_to_gain, Routing, OUT_CHANNELS, SAMPLE_RATE};
use crate::options::ModuleOptions;
use crate::realtime::{Source, Streamer};

pub struct Args {
    pub input: PathBuf,
    pub output: PathBuf,
    pub module: ModuleOptions,
    pub quantum: Option<usize>,
    pub tail_ms: u32,
    pub stereo_bypass: bool,
    pub gain_db: Option<f32>,
    pub volume_db: f32,
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
        let mut module = ModuleOptions::default();
        let mut tail_ms = 500;
        let mut quantum = None;
        let mut stereo_bypass = false;
        let mut gain_db = None;
        let mut volume_db = 0.0f32;
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            let mut val = || it.next().with_context(|| format!("manca il valore di {a}"));
            if module.parse_flag(&a, &mut val)? {
                continue;
            }
            match a.as_str() {
                "--quantum" => quantum = Some(val()?.parse::<usize>()?).filter(|&q| q > 0),
                "--stereo-bypass" => stereo_bypass = true,
                "--tail-ms" => tail_ms = val()?.parse()?,
                "--gain-db" => gain_db = Some(val()?.parse()?),
                "--volume-db" => volume_db = val()?.parse()?,
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
        Ok(Args { input, output, module, quantum, tail_ms, stereo_bypass, gain_db, volume_db })
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
    let in_ch = spec.channels as usize;

    let mut engine = args.module.open::<B>()?;
    eprintln!("modulo DAP configurato ({:?}, blocco {} frame, {} canali in ingresso)", args.module.mode, args.module.block, in_ch);

    let frames_in = input.len() / in_ch;
    let tail = (args.tail_ms as usize * SAMPLE_RATE as usize) / 1000;
    let total = frames_in + tail;
    let block = engine.block_frames();
    // Windows applica il volume dell'endpoint dopo il modulo.
    let volume = db_to_gain(args.volume_db);
    let out = match args.quantum {
        None => {
            if routing.gain != 1.0 {
                input.iter_mut().for_each(|s| *s *= routing.gain);
            }
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
            if volume != 1.0 {
                out.iter_mut().for_each(|s| *s *= volume);
            }
            out
        }
        Some(q) => stream(engine, &input, in_ch, &routing, total.div_ceil(block) * block, q, volume)?,
    };

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

/// Lo stesso render attraverso [`Streamer`], a cicli di `quantum` frame come nel nodo PipeWire:
/// restituisce `frames` frame interleaved, tolti gli zeri della latenza dell'adattatore.
fn stream<B: Backend>(engine: Engine<B>, input: &[f32], in_ch: usize, routing: &Routing, frames: usize, quantum: usize, volume: f32) -> Result<Vec<f32>> {
    let frames_in = input.len() / in_ch;
    let planar: Vec<Vec<f32>> = (0..in_ch).map(|c| input.iter().skip(c).step_by(in_ch).copied().collect()).collect();
    let mut streamer = Streamer::new(engine, quantum);
    let (mut left, mut right) = (vec![0.0f32; quantum], vec![0.0f32; quantum]);
    let mut out = Vec::with_capacity((frames + quantum) * OUT_CHANNELS);
    let mut pos = 0;
    while out.len() < (frames + streamer.latency()) * OUT_CHANNELS {
        let from = pos.min(frames_in);
        let to = (pos + quantum).min(frames_in);
        let chans: Vec<&[f32]> = planar.iter().map(|c| &c[from..to]).collect();
        streamer.process(quantum, &[Source { routing, channels: &chans }], volume, [&mut left, &mut right])?;
        out.extend(left.iter().zip(&right).flat_map(|(&l, &r)| [l, r]));
        pos += quantum;
    }
    let latency = streamer.latency();
    eprintln!("adattatore del tempo reale: cicli di {quantum} frame, latenza {latency} frame");
    if out[..latency * OUT_CHANNELS].iter().any(|&s| s != 0.0) {
        bail!("l'adattatore ha prodotto audio nella latenza");
    }
    out.drain(..latency * OUT_CHANNELS);
    out.truncate(frames * OUT_CHANNELS);
    Ok(out)
}
