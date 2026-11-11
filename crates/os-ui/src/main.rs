//! os-ui: la finestra di open-surround con gli stessi controlli di Dolby Access (fase 6, doc
//! 12.15, passo L3).
//!
//! Lo stato è un [`Settings`] del modello: a ogni cambio la finestra salva il file delle
//! impostazioni e manda al nodo `os-pipewire` il comando `profilo <stato>` sul socket di
//! controllo; ogni 2 s chiede `stato` al nodo e lo mostra. Al primo contatto con il nodo, se il
//! suo profilo è diverso da quello delle impostazioni, gli manda quello delle impostazioni.
//!
//! Uso:
//!   os-ui [opzioni]
//!     --settings <file>   file delle impostazioni (default ~/.config/open-surround/settings.json)
//!     --socket <path>     socket del nodo (default $XDG_RUNTIME_DIR/open-surround/control.sock)
//!   Per le prove:
//!     --select <profilo>  profilo attivo all'avvio (gaming, film, …, custom3), senza salvare
//!     --snapshot <f.ppm>  salva un'immagine della finestra dopo l'avvio ed esce

mod app;
mod view;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use os_profiles::control;
use os_profiles::settings::{default_path, Kind, Settings};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

use app::App;
use view::Throttle;

slint::include_modules!();

/// Intervallo minimo fra due invii mentre si trascina uno slider del GEQ.
const GEQ_INTERVAL: Duration = Duration::from_millis(100);
/// Ogni quanto chiedere lo stato al nodo.
const POLL: Duration = Duration::from_secs(2);

struct Args {
    settings: PathBuf,
    socket: PathBuf,
    select: Option<Kind>,
    snapshot: Option<PathBuf>,
}

impl Args {
    fn parse() -> Result<Args> {
        let mut a = Args { settings: default_path()?, socket: control::default_socket_path()?, select: None, snapshot: None };
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            let mut val = || it.next().with_context(|| format!("manca il valore di {flag}"));
            match flag.as_str() {
                "--settings" => a.settings = val()?.into(),
                "--socket" => a.socket = val()?.into(),
                "--select" => a.select = Some(val()?.parse()?),
                "--snapshot" => a.snapshot = Some(val()?.into()),
                "-h" | "--help" => {
                    let usage: Vec<&str> = include_str!("main.rs").lines().take_while(|l| l.starts_with("//!")).map(|l| l.trim_start_matches("//!")).collect();
                    eprintln!("{}", usage.join("\n"));
                    std::process::exit(0);
                }
                _ => bail!("opzione sconosciuta: {flag} (vedi --help)"),
            }
        }
        Ok(a)
    }
}

/// Copia lo stato di `Settings` nelle proprietà della finestra.
fn refresh(ui: &AppWindow, app: &App) {
    let s = &app.settings;
    ui.set_active(view::kind_index(s.active_kind()));
    ui.set_performance(s.performance());
    if let Some(ieq) = s.ieq() {
        ui.set_ieq(view::ieq_index(ieq));
    }
    if let Some(c) = s.custom() {
        ui.set_virtualizer(c.virtualizer);
        ui.set_leveler(c.leveler);
        let labels = view::band_labels();
        let bands: Vec<Band> = c.geq.iter().zip(labels).map(|(&v, hz)| Band { hz: hz.into(), value: v as f32 }).collect();
        ui.set_geq(ModelRc::new(VecModel::from(bands)));
    }
    ui.set_node_ok(app.connected);
    ui.set_error(SharedString::from(app.error.as_str()));
}

fn main() -> Result<()> {
    let args = Args::parse()?;
    let (settings, error) = match Settings::load(&args.settings) {
        Ok(s) => (s, String::new()),
        Err(e) => (Settings::default(), format!("Impostazioni illeggibili, uso i default: {e:#}")),
    };
    let mut settings = settings;
    if let Some(k) = args.select {
        settings.select(k);
    }
    let app = Rc::new(RefCell::new(App {
        settings,
        path: args.settings.clone(),
        socket: args.socket.clone(),
        sent: None,
        connected: false,
        throttle: Throttle::new(GEQ_INTERVAL),
        flush_pending: false,
        error,
    }));

    let ui = AppWindow::new()?;
    let names: Vec<SharedString> = view::PROFILE_NAMES.iter().map(|&n| n.into()).collect();
    ui.set_profiles(ModelRc::new(VecModel::from(names)));
    {
        let mut a = app.borrow_mut();
        let status = a.poll();
        ui.set_node_status(status.into());
        refresh(&ui, &a);
    }

    // Un'azione dell'utente: cambia Settings, aggiorna la finestra, salva e manda al nodo.
    // `throttled` per gli slider: al più un invio ogni GEQ_INTERVAL.
    let act = {
        let (app, weak) = (app.clone(), ui.as_weak());
        Rc::new(move |f: &dyn Fn(&mut Settings) -> Result<()>, throttled: bool| {
            let Some(ui) = weak.upgrade() else { return };
            let mut a = app.borrow_mut();
            if let Err(e) = f(&mut a.settings) {
                a.error = format!("{e:#}");
                refresh(&ui, &a);
                return;
            }
            let delay = if throttled { a.throttle.delay(Instant::now()) } else { Duration::ZERO };
            if delay.is_zero() {
                a.flush();
            } else if !a.flush_pending {
                a.flush_pending = true;
                let (app, weak) = (app.clone(), weak.clone());
                Timer::single_shot(delay, move || {
                    let mut a = app.borrow_mut();
                    a.flush_pending = false;
                    a.flush();
                    if let Some(ui) = weak.upgrade() {
                        refresh(&ui, &a);
                    }
                });
            }
            refresh(&ui, &a);
        })
    };

    let f = act.clone();
    ui.on_select(move |i| {
        if let Some(k) = view::kind_at(i) {
            f(&|s| {
                s.select(k);
                Ok(())
            }, false);
        }
    });
    let f = act.clone();
    ui.on_set_performance(move |on| f(&|s| s.set_performance(on), false));
    let f = act.clone();
    ui.on_set_ieq(move |i| {
        if let Some(ieq) = view::ieq_at(i) {
            f(&|s| s.set_ieq(ieq), false);
        }
    });
    let f = act.clone();
    ui.on_set_virtualizer(move |on| f(&|s| s.set_virtualizer(on), false));
    let f = act.clone();
    ui.on_set_leveler(move |on| f(&|s| s.set_leveler(on), false));
    let f = act.clone();
    ui.on_set_geq(move |band, v| {
        let value = view::slider_value(v);
        f(&|s| s.set_geq(band as usize, value).map(|_| ()), true);
    });
    let f = act.clone();
    ui.on_reset(move || {
        f(&|s| {
            s.reset();
            Ok(())
        }, false)
    });

    // Stato del nodo ogni POLL.
    let poll = Timer::default();
    {
        let (app, weak) = (app.clone(), ui.as_weak());
        poll.start(TimerMode::Repeated, POLL, move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut a = app.borrow_mut();
            let status = a.poll();
            ui.set_node_status(status.into());
            refresh(&ui, &a);
        });
    }

    if let Some(path) = args.snapshot.clone() {
        let weak = ui.as_weak();
        Timer::single_shot(Duration::from_millis(800), move || {
            if let Some(ui) = weak.upgrade() {
                if let Err(e) = snapshot(&ui, &path) {
                    eprintln!("istantanea: {e:#}");
                }
            }
            let _ = slint::quit_event_loop();
        });
    }

    ui.run()?;
    Ok(())
}

/// Immagine della finestra in PPM (P6), senza dipendenze in più: per le prove.
fn snapshot(ui: &AppWindow, path: &Path) -> Result<()> {
    let buf = ui.window().take_snapshot()?;
    let mut out = format!("P6\n{} {}\n255\n", buf.width(), buf.height()).into_bytes();
    for px in buf.as_slice() {
        out.extend_from_slice(&[px.r, px.g, px.b]);
    }
    std::fs::write(path, out).with_context(|| path.display().to_string())?;
    eprintln!("istantanea {}x{} in {}", buf.width(), buf.height(), path.display());
    Ok(())
}
