//! os-pw-latency: misura nel grafo PipeWire la latenza del nodo os-pipewire (fase 4, doc sez.
//! 12.13). Manda impulsi in un sink del nodo e li cerca sul monitor del sink su cui il nodo
//! suona; la distanza si conta in frame sul clock del grafo (`clock.position` del driver, lo
//! stesso per i due flussi), quindi non dipende da quando partono i processi.
//!
//! Il valore misurato è quello che il nodo aggiunge nel grafo: ritardo del modulo (512 frame,
//! il picco della risposta all'impulso) + latenza dell'adattatore + eventuali cicli in più del
//! grafo, più un ciclo dello strumento stesso: con `--sink` e `--monitor` sullo stesso null sink,
//! senza nodo, misura un quantum (1024 frame il 4/11/2026). Non comprende il buffer del
//! dispositivo, che PipeWire riporta a parte (pw-top). Va usato con un profilo senza leveler
//! (Gaming performance): con il leveler i picchi di impulsi ravvicinati non sono affidabili.
//!
//! Uso:
//!   os-pw-latency [--sink <nodo>] [--monitor <sink>] [--count N] [--period-ms N] [--channels N]
//!     --sink <nodo>      sink in cui suonare gli impulsi (default open-surround-7.1)
//!     --monitor <sink>   sink di cui registrare il monitor: quello su cui suona l'uscita del nodo
//!     --count N          impulsi (default 10)
//!     --period-ms N      distanza fra gli impulsi (default 500)
//!     --channels N       canali del flusso di prova, impulso sul primo (default 2)

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use anyhow::{bail, Context, Result};
    use pipewire as pw;
    use pw::spa;
    use pw::spa::pod::{serialize::PodSerializer, Object, Pod, Value};
    use pw::spa::sys as spa_sys;
    use pw::stream::{StreamBox, StreamFlags};

    const RATE: u32 = 48_000;
    /// Soglia di rilevamento sul monitor (l'impulso è a 0,5, il modulo lo attenua di decine di dB).
    const THRESHOLD: f32 = 1e-4;

    struct Args {
        sink: String,
        monitor: Option<String>,
        count: usize,
        period: u64,
        channels: u32,
    }

    #[derive(Default)]
    struct State {
        /// Frame (sul clock del grafo) degli impulsi mandati.
        sent: Vec<u64>,
        next: Option<u64>,
        /// Per ogni impulso mandato: frame e ampiezza del picco più alto visto dopo.
        peaks: Vec<(u64, f32)>,
        done: bool,
    }

    fn parse() -> Result<Args> {
        let mut a = Args { sink: "open-surround-7.1".into(), monitor: None, count: 10, period: 24_000, channels: 2 };
        let mut it = std::env::args().skip(1);
        while let Some(f) = it.next() {
            let mut val = || it.next().with_context(|| format!("manca il valore di {f}"));
            match f.as_str() {
                "--sink" => a.sink = val()?,
                "--monitor" => a.monitor = Some(val()?),
                "--count" => a.count = val()?.parse()?,
                "--period-ms" => a.period = val()?.parse::<u64>()? * RATE as u64 / 1000,
                "--channels" => a.channels = val()?.parse()?,
                "-h" | "--help" => {
                    eprintln!("{}", include_str!("os-pw-latency.rs").lines().take_while(|l| l.starts_with("//!")).map(|l| l.trim_start_matches("//!")).collect::<Vec<_>>().join("\n"));
                    std::process::exit(0);
                }
                _ => bail!("opzione sconosciuta: {f}"),
            }
        }
        Ok(a)
    }

    fn format(channels: u32) -> Vec<u8> {
        let mut info = spa::param::audio::AudioInfoRaw::new();
        info.set_format(spa::param::audio::AudioFormat::F32P);
        info.set_rate(RATE);
        info.set_channels(channels);
        let order = [
            spa_sys::SPA_AUDIO_CHANNEL_FL,
            spa_sys::SPA_AUDIO_CHANNEL_FR,
            spa_sys::SPA_AUDIO_CHANNEL_FC,
            spa_sys::SPA_AUDIO_CHANNEL_LFE,
            spa_sys::SPA_AUDIO_CHANNEL_RL,
            spa_sys::SPA_AUDIO_CHANNEL_RR,
            spa_sys::SPA_AUDIO_CHANNEL_SL,
            spa_sys::SPA_AUDIO_CHANNEL_SR,
        ];
        let mut pos = [0u32; spa_sys::SPA_AUDIO_MAX_CHANNELS as usize];
        pos[..channels as usize].copy_from_slice(&order[..channels as usize]);
        info.set_position(pos);
        PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &Value::Object(Object { type_: spa_sys::SPA_TYPE_OBJECT_Format, id: spa_sys::SPA_PARAM_EnumFormat, properties: info.into() }),
        )
        .expect("pod")
        .0
        .into_inner()
    }

    /// Posizione e durata del ciclo corrente sul clock del driver.
    unsafe fn clock(p: &AtomicPtr<spa_sys::spa_io_position>) -> Option<(u64, usize)> {
        let p = p.load(Ordering::Acquire);
        (!p.is_null()).then(|| unsafe { ((*p).clock.position, (*p).clock.duration as usize) })
    }

    pub fn run() -> Result<()> {
        let args = parse()?;
        pw::init();
        let mainloop = pw::main_loop::MainLoopRc::new(None)?;
        let context = pw::context::ContextRc::new(&mainloop, None)?;
        let core = context.connect_rc(None)?;
        let state = Arc::new(Mutex::new(State::default()));
        let finished = Arc::new(AtomicBool::new(false));
        let pos_play = Arc::new(AtomicPtr::new(std::ptr::null_mut()));
        let pos_rec = Arc::new(AtomicPtr::new(std::ptr::null_mut()));

        let mut props = pw::properties::PropertiesBox::new();
        props.insert(*pw::keys::MEDIA_TYPE, "Audio");
        props.insert(*pw::keys::MEDIA_CATEGORY, "Playback");
        props.insert(*pw::keys::NODE_NAME, "os-pw-latency-play");
        props.insert("target.object", args.sink.as_str());
        props.insert("channelmix.upmix", "false");
        let play = StreamBox::new(&core, "os-pw-latency-play", props)?;
        let mut props = pw::properties::PropertiesBox::new();
        props.insert(*pw::keys::MEDIA_TYPE, "Audio");
        props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
        props.insert(*pw::keys::NODE_NAME, "os-pw-latency-rec");
        if let Some(m) = &args.monitor {
            props.insert("target.object", m.as_str());
        }
        props.insert("stream.capture.sink", "true");
        let rec = StreamBox::new(&core, "os-pw-latency-rec", props)?;

        let (st, pp) = (state.clone(), pos_play.clone());
        let (count, period) = (args.count, args.period);
        let pp_io = pos_play.clone();
        let _lp = play
            .add_local_listener_with_user_data(())
            .io_changed(move |_, _, id, area, _| {
                if id == spa_sys::SPA_IO_Position {
                    pp_io.store(area as *mut spa_sys::spa_io_position, Ordering::Release);
                }
            })
            .process(move |stream, _| {
                let Some(mut buf) = stream.dequeue_buffer() else { return };
                let Some((now, duration)) = (unsafe { clock(&pp) }) else { return };
                let mut st = st.lock().unwrap();
                let datas = buf.datas_mut();
                // Un ciclo esatto: con il buffer intero gli impulsi finirebbero nei cicli dopo.
                let n = datas.first_mut().and_then(|d| d.data()).map_or(0, |d| d.len() / 4).min(duration);
                for (c, d) in datas.iter_mut().enumerate() {
                    if let Some(slice) = d.data() {
                        let s: &mut [f32] = unsafe { std::slice::from_raw_parts_mut(slice.as_mut_ptr() as *mut f32, n) };
                        s.fill(0.0);
                        if c == 0 {
                            // Primo impulso dopo un secondo, poi ogni `period` frame.
                            let next = *st.next.get_or_insert(now + RATE as u64);
                            if st.sent.len() < count && next >= now && next < now + n as u64 {
                                s[(next - now) as usize] = 0.5;
                                st.sent.push(next);
                                st.peaks.push((0, 0.0));
                                st.next = Some(next + period);
                            }
                        }
                    }
                    let chunk = d.chunk_mut();
                    *chunk.offset_mut() = 0;
                    *chunk.stride_mut() = 4;
                    *chunk.size_mut() = (n * 4) as u32;
                }
            })
            .register()?;

        let (st, pr, fin) = (state.clone(), pos_rec.clone(), finished.clone());
        let pr_io = pos_rec.clone();
        let _lr = rec
            .add_local_listener_with_user_data(())
            .io_changed(move |_, _, id, area, _| {
                if id == spa_sys::SPA_IO_Position {
                    pr_io.store(area as *mut spa_sys::spa_io_position, Ordering::Release);
                }
            })
            .process(move |stream, _| {
                let Some(mut buf) = stream.dequeue_buffer() else { return };
                let Some((now, _)) = (unsafe { clock(&pr) }) else { return };
                let mut st = st.lock().unwrap();
                for d in buf.datas_mut().iter_mut().take(2) {
                    let size = d.chunk().size() as usize / 4;
                    let Some(slice) = d.data() else { continue };
                    let s: &[f32] = unsafe { std::slice::from_raw_parts(slice.as_ptr() as *const f32, size) };
                    for (j, &v) in s.iter().enumerate() {
                        let t = now + j as u64;
                        // Il picco appartiene all'ultimo impulso mandato prima di t.
                        if let Some(k) = st.sent.iter().rposition(|&x| x <= t) {
                            if t < st.sent[k] + period / 2 && v.abs() > st.peaks[k].1 {
                                st.peaks[k] = (t, v.abs());
                            }
                        }
                    }
                }
                if st.sent.len() == count && st.sent.last().is_some_and(|&l| now > l + period) {
                    st.done = true;
                    fin.store(true, Ordering::Release);
                }
            })
            .register()?;

        let flags = StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS;
        let f = format(args.channels);
        play.connect(spa::utils::Direction::Output, None, flags, &mut [Pod::from_bytes(&f).unwrap()])?;
        let f2 = format(2);
        rec.connect(spa::utils::Direction::Input, None, flags, &mut [Pod::from_bytes(&f2).unwrap()])?;

        let ml = mainloop.clone();
        let fin = finished.clone();
        let timeout = std::cell::Cell::new(0u32);
        let limit = 50 + (args.count as u64 * args.period * 10 / RATE as u64) as u32;
        let timer = mainloop.loop_().add_timer(move |_| {
            timeout.set(timeout.get() + 1);
            if fin.load(Ordering::Acquire) || timeout.get() > limit {
                ml.quit();
            }
        });
        timer.update_timer(Some(Duration::from_millis(100)), Some(Duration::from_millis(100))).into_result()?;
        mainloop.run();

        let st = state.lock().unwrap();
        if !st.done {
            bail!("misura incompleta: {} impulsi mandati su {}, monitor senza dati? (--monitor)", st.sent.len(), args.count);
        }
        let mut lat: Vec<i64> = Vec::new();
        for (s, (t, a)) in st.sent.iter().zip(&st.peaks) {
            if *a > THRESHOLD {
                lat.push(*t as i64 - *s as i64);
            }
        }
        if lat.is_empty() {
            bail!("nessun impulso rilevato sul monitor");
        }
        lat.sort();
        let ms = |f: i64| f as f64 * 1000.0 / RATE as f64;
        println!(
            "impulsi rilevati {} su {}: latenza nel grafo {} frame ({:.2} ms), min {} max {}",
            lat.len(),
            st.sent.len(),
            lat[lat.len() / 2],
            ms(lat[lat.len() / 2]),
            lat[0],
            lat[lat.len() - 1]
        );
        Ok(())
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn main() -> anyhow::Result<()> {
    imp::run()
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn main() {
    eprintln!("os-pw-latency è per Linux x86-64");
    std::process::exit(1);
}
