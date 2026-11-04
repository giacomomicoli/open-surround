//! Controllo della riconfigurazione (fase 4, doc sez. 12.13): elabora `--warmup-s` secondi
//! dell'ingresso, chiama ConfigureEncoder di nuovo e poi elabora l'ingresso intero come
//! l'offline (stessa catena, stessa coda). Se la riconfigurazione azzera lo stato del modulo,
//! l'uscita è identica byte per byte a quella di `os-dap-linux` con le stesse opzioni.
//!
//! Con `--resets N` riconfigura N volte di seguito e riporta tempo per chiamata e memoria
//! residente prima e dopo (la riconfigurazione gira nel thread real time del nodo).
//!
//! Uso: cargo run --example reset_check -- [--warmup-s N] [--resets N] <opzioni di os-dap-linux> in.wav out.wav

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn main() {
    eprintln!("reset_check usa il backend Linux x86-64");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn main() -> anyhow::Result<()> {
    use os_dap_core::layout::{db_to_gain, Routing, OUT_CHANNELS, SAMPLE_RATE};
    use os_dap_core::offline::{read_wav, Args};

    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    let mut warmup_s = 5.0f64;
    if let Some(i) = argv.iter().position(|a| a == "--warmup-s") {
        warmup_s = argv[i + 1].parse()?;
        argv.drain(i..=i + 1);
    }
    let mut resets = 1usize;
    if let Some(i) = argv.iter().position(|a| a == "--resets") {
        resets = argv[i + 1].parse()?;
        argv.drain(i..=i + 1);
    }
    let args = Args::parse(argv)?;
    let (spec, mut input) = read_wav(&args.input)?;
    let routing = Routing::new(spec.channels, args.stereo_bypass, args.gain_db)?;
    input.iter_mut().for_each(|s| *s *= routing.gain);
    let in_ch = spec.channels as usize;
    let frames_in = input.len() / in_ch;
    let mut engine = args.module.open::<os_dap_linux::LinuxBackend>()?;
    let block = engine.block_frames();

    let render = |engine: &mut os_dap_core::Engine<_>, total: usize| -> anyhow::Result<Vec<f32>> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < total {
            for (c, &b) in routing.map.iter().enumerate() {
                for (i, s) in engine.input_mut(b).iter_mut().enumerate() {
                    let f = pos + i;
                    *s = if f < frames_in { input[f * in_ch + c] } else { 0.0 };
                }
            }
            out.extend_from_slice(engine.process(&routing.active)?);
            pos += block;
        }
        Ok(out)
    };
    render(&mut engine, (warmup_s * SAMPLE_RATE as f64) as usize)?;
    let rss = || std::fs::read_to_string("/proc/self/statm").ok().and_then(|s| s.split(' ').nth(1)?.parse::<u64>().ok()).unwrap_or(0) * 4;
    let before = rss();
    let mut worst = std::time::Duration::ZERO;
    let t0 = std::time::Instant::now();
    for _ in 0..resets {
        let t = std::time::Instant::now();
        engine.reconfigure()?;
        worst = worst.max(t.elapsed());
    }
    eprintln!(
        "riconfigurato {resets} volte dopo {warmup_s} s: media {:.3} ms, massimo {:.3} ms; memoria residente {before} → {} KiB",
        t0.elapsed().as_secs_f64() * 1000.0 / resets as f64,
        worst.as_secs_f64() * 1000.0,
        rss()
    );
    let tail = (args.tail_ms as usize * SAMPLE_RATE as usize) / 1000;
    let mut out = render(&mut engine, frames_in + tail)?;
    let volume = db_to_gain(args.volume_db);
    if volume != 1.0 {
        out.iter_mut().for_each(|s| *s *= volume);
    }
    let mut w = hound::WavWriter::create(
        &args.output,
        hound::WavSpec { channels: OUT_CHANNELS as u16, sample_rate: SAMPLE_RATE, bits_per_sample: 32, sample_format: hound::SampleFormat::Float },
    )?;
    for s in &out {
        w.write_sample(*s)?;
    }
    w.finalize()?;
    Ok(())
}
