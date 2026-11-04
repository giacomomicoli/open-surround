//! wavcmp: confronta il riferimento registrato su Windows con l'uscita di os-dap-host.
//!
//! Allinea i due file (ritardo stimato con correlazione incrociata sul canale sinistro),
//! stima il guadagno relativo ai minimi quadrati e riporta il residuo per canale, sia con
//! guadagno unitario sia con quello stimato. Residuo esattamente zero = bit-identico.
//!
//! Uso: wavcmp [--max-lag-ms N] [--onset] [--segments S] [--residual F] <riferimento.wav> <prova.wav>
//!   --onset        pre-allinea sull'attacco (primo campione sopra −60 dB dal picco) e cerca il
//!                  ritardo fine attorno a quello: per registrazioni che iniziano in un momento
//!                  qualsiasi (nodo PipeWire, fase 4)
//!   --segments S   ripete la stima di guadagno e residuo su segmenti di S secondi
//!   --residual F   scrive in F il residuo allineato (rif - guadagno stimato · prova)

use std::path::Path;

use anyhow::{bail, Context, Result};

fn read(path: &Path) -> Result<(usize, u32, Vec<Vec<f32>>)> {
    let mut r = hound::WavReader::open(path).with_context(|| format!("apertura {}", path.display()))?;
    let spec = r.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    let ch = spec.channels as usize;
    let chans = (0..ch).map(|c| interleaved.iter().skip(c).step_by(ch).copied().collect()).collect();
    Ok((ch, spec.sample_rate, chans))
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

fn rms(x: impl Iterator<Item = f64>) -> f64 {
    let (s, n) = x.fold((0.0, 0usize), |(s, n), v| (s + v * v, n + 1));
    (s / n.max(1) as f64).sqrt()
}

/// Primo campione sopra −60 dB dal picco del canale.
fn onset(x: &[f32]) -> usize {
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    x.iter().position(|v| v.abs() > peak * 1e-3).unwrap_or(0)
}

/// Ritardo `lag` tale che prova[i] ≈ rif[i + lag]; cerca in [center - max, center + max].
fn best_lag(reference: &[f32], test: &[f32], center: isize, max: isize) -> (isize, f64) {
    // Finestra sulla parte più energetica della prova, per ridurre i costi, scelta fra quelle
    // che restano dentro il riferimento per tutti i ritardi cercati (altrimenti i ritardi
    // vicini a quello giusto verrebbero scartati).
    let win = (test.len() / 2).min(reference.len() / 2).min(5 * 48_000);
    let block = 4800;
    let lo = (max - center).max(0) as usize;
    let hi = (reference.len() as isize - win as isize - center - max).min(test.len() as isize - win as isize);
    let (lo, hi) = if hi >= lo as isize { (lo, hi as usize) } else { (0, test.len().saturating_sub(win)) };
    let start = (lo / block..=hi / block)
        .filter(|k| k * block >= lo && k * block <= hi)
        .max_by(|&a, &b| {
            let e = |k: usize| test[k * block..k * block + block].iter().map(|v| v * v).sum::<f32>();
            e(a).total_cmp(&e(b))
        })
        .unwrap_or(lo)
        * block;
    let seg = &test[start..start + win];
    let norm_t = seg.iter().map(|v| (*v as f64).powi(2)).sum::<f64>().sqrt();
    let mut best = (0isize, f64::MIN);
    for lag in center - max..=center + max {
        let off = start as isize + lag;
        if off < 0 || off as usize + win > reference.len() {
            continue;
        }
        let r = &reference[off as usize..off as usize + win];
        let dot: f64 = r.iter().zip(seg).map(|(a, b)| *a as f64 * *b as f64).sum();
        let norm_r = r.iter().map(|v| (*v as f64).powi(2)).sum::<f64>().sqrt();
        let c = dot / (norm_r * norm_t).max(1e-20);
        if c > best.1 {
            best = (lag, c);
        }
    }
    best
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut max_lag_ms = 500.0;
    if let Some(i) = args.iter().position(|a| a == "--max-lag-ms") {
        max_lag_ms = args.get(i + 1).context("manca il valore di --max-lag-ms")?.parse()?;
        args.drain(i..=i + 1);
    }
    let align_onset = args.iter().any(|a| a == "--onset");
    args.retain(|a| a != "--onset");
    let mut segment = None;
    if let Some(i) = args.iter().position(|a| a == "--segments") {
        segment = Some(args.get(i + 1).context("manca il valore di --segments")?.parse::<f64>()?);
        args.drain(i..=i + 1);
    }
    let mut residual_out = None;
    if let Some(i) = args.iter().position(|a| a == "--residual") {
        residual_out = Some(args.get(i + 1).context("manca il valore di --residual")?.clone());
        args.drain(i..=i + 1);
    }
    let [ref_path, test_path] = <[String; 2]>::try_from(args)
        .map_err(|_| anyhow::anyhow!("uso: wavcmp [--max-lag-ms N] <riferimento.wav> <prova.wav>"))?;
    let (rc, rr, reference) = read(Path::new(&ref_path))?;
    let (tc, tr, test) = read(Path::new(&test_path))?;
    if rr != tr {
        bail!("frequenze diverse: {rr} vs {tr} Hz");
    }
    let ch = rc.min(tc);
    let max = (max_lag_ms * rr as f64 / 1000.0) as isize;
    let center = if align_onset { onset(&reference[0]) as isize - onset(&test[0]) as isize } else { 0 };
    let (lag, corr) = best_lag(&reference[0], &test[0], center, max);
    println!(
        "ritardo: {lag} campioni ({:.2} ms, rif. rispetto a prova), correlazione {corr:.6}",
        lag as f64 * 1000.0 / rr as f64
    );

    // Intervallo comune dopo l'allineamento.
    let t0 = (-lag).max(0) as usize;
    let len = (reference[0].len() as isize - lag - t0 as isize).min((test[0].len() - t0) as isize).max(0) as usize;
    let (reference, test) = (&reference, &test);
    let pairs = |c: usize| (t0..t0 + len).map(move |i| (reference[c][(i as isize + lag) as usize] as f64, test[c][i] as f64));
    let mut gains = Vec::with_capacity(ch);

    for c in 0..ch {
        let (dot, tt) = pairs(c).fold((0.0, 0.0), |(d, t), (r, s)| (d + r * s, t + s * s));
        let gain = dot / tt.max(1e-20);
        let r_rms = rms(pairs(c).map(|(r, _)| r));
        let res1 = rms(pairs(c).map(|(r, s)| r - s));
        let resg = rms(pairs(c).map(|(r, s)| r - gain * s));
        let maxdiff = pairs(c).map(|(r, s)| (r - s).abs()).fold(0.0, f64::max);
        let identical = pairs(c).all(|(r, s)| r == s);
        if let Some(seg) = segment {
            let n = (seg * rr as f64) as usize;
            for (k, start) in (0..len).step_by(n.max(1)).enumerate() {
                let p: Vec<(f64, f64)> = pairs(c).skip(start).take(n).collect();
                let (dot, tt) = p.iter().fold((0.0, 0.0), |(d, t), (r, s)| (d + r * s, t + s * s));
                let g = dot / tt.max(1e-20);
                let r_rms = rms(p.iter().map(|(r, _)| *r));
                let res = rms(p.iter().map(|(r, s)| r - g * s));
                if r_rms > 1e-6 {
                    println!(
                        "  canale {c} segmento {k}: rif {:.1} dBFS, guadagno {:+.2} dB, residuo {:.1} dB rel.",
                        db(r_rms), db(g.abs()), db(res / r_rms)
                    );
                }
            }
        }
        println!(
            "canale {c}: rif {:.1} dBFS | residuo g=1 {:.1} dB rel. | guadagno stimato {:+.3} dB, residuo {:.1} dB rel. | diff max {:.2e}{}",
            db(r_rms),
            db(res1 / r_rms.max(1e-20)),
            db(gain.abs()),
            db(resg / r_rms.max(1e-20)),
            maxdiff,
            if identical { " | BIT-IDENTICO" } else { "" }
        );
        gains.push(gain);
    }
    if let Some(path) = residual_out {
        // Residuo allineato rif - g·prova, con il guadagno stimato per canale.
        let mut w = hound::WavWriter::create(
            &path,
            hound::WavSpec { channels: ch as u16, sample_rate: rr, bits_per_sample: 32, sample_format: hound::SampleFormat::Float },
        )?;
        for i in t0..t0 + len {
            for (c, g) in gains.iter().enumerate() {
                let r = reference[c][(i as isize + lag) as usize] as f64;
                w.write_sample((r - g * test[c][i] as f64) as f32)?;
            }
        }
        w.finalize()?;
        println!("residuo scritto in {path}");
    }
    Ok(())
}
