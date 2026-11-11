//! Passo L1 della fase 6 (doc 12.15): la logica dell'interfaccia ([`Settings`]).
//!
//! Criterio 1: ognuno dei 36 stati catturati (i 19 di `target/verify`, se presente, e le 17
//! catture `w1-*`), raggiunto **con le azioni dell'interfaccia**, genera i sei file
//! dell'endpoint identici byte per byte a quelli scritti da Dolby Access.
//! Criterio 2: memoria per profilo, "Predefinito" sul solo profilo attivo (con i default
//! catturati in W1), limiti del GEQ, controlli che un profilo non ha.

use std::path::{Path, PathBuf};

use os_profiles::model::{Custom, Ieq, Profile, GEQ_RANGE};
use os_profiles::settings::{Kind, Settings};
use os_profiles::DolbyContext;

const ENDPOINT: &str = "32ed16b6";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// I 19 stati di 12.9 in `target/verify` (vuoto se la cartella manca).
fn stati_verify() -> Vec<PathBuf> {
    let verify = root().join("target/verify");
    if !verify.is_dir() {
        eprintln!("target/verify assente: solo le catture w1-* (scaricare l'asset verify-data-2026-11-03)");
        return vec![];
    }
    [
        "gaming-performance", "gaming-perfoff-ieq-off", "gaming-balanced", "gaming-detailed", "gaming-warm",
        "film-balanced", "film-detailed", "film-warm", "film-ieq-off",
        "music-balanced", "music-detailed", "music-warm", "music-ieq-off",
        "voice", "custom1-virt-on", "custom1-virt-off", "custom1-leveler-off", "custom2-default", "custom3-default",
    ]
    .iter()
    .map(|n| verify.join(n).join("canali71"))
    .collect()
}

fn catture_w1() -> Vec<PathBuf> {
    let dir = root().join("docs/reference/captures");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir() && p.file_name().unwrap().to_string_lossy().starts_with("w1-"))
        .collect();
    dirs.sort();
    dirs
}

/// Lo stato di una cattura: `import.txt` per W1, l'importazione per `target/verify` (che il
/// test `stati.rs` controlla contro la tabella dei 19 stati).
fn stato(dir: &Path) -> Profile {
    match std::fs::read_to_string(dir.join("import.txt")) {
        Ok(t) => t.trim_start_matches('\u{feff}').trim().parse().unwrap(),
        Err(_) => Profile::import(&DolbyContext::read_capture(dir, ENDPOINT).unwrap()).unwrap(),
    }
}

/// Porta l'interfaccia allo stato `target` con le sole azioni che ha un utente: scegliere il
/// profilo e muovere i controlli che l'app mostra per quel profilo.
fn usa_i_controlli(s: &mut Settings, target: &Profile) {
    s.select(Kind::of(target));
    match *target {
        Profile::Gaming { ieq: Ieq::Off } => s.set_performance(true).unwrap(),
        Profile::Gaming { ieq } => {
            s.set_performance(false).unwrap();
            s.set_ieq(ieq).unwrap();
        }
        Profile::Film { ieq } | Profile::Music { ieq } => s.set_ieq(ieq).unwrap(),
        Profile::Voice => {}
        Profile::Custom { settings: c, .. } => {
            s.set_virtualizer(c.virtualizer).unwrap();
            s.set_leveler(c.leveler).unwrap();
            for (band, &v) in c.geq.iter().enumerate() {
                assert_eq!(s.set_geq(band, v).unwrap(), v);
            }
        }
    }
}

fn file_identici(nome: &str, profile: &Profile, dir: &Path) {
    for (file, bytes) in profile.context().capture_files(ENDPOINT) {
        let captured = std::fs::read(dir.join(&file)).unwrap();
        assert!(captured == bytes, "{nome}: {file} generato diverso da quello catturato");
    }
}

/// Criterio 1. Ogni stato si raggiunge due volte: dai default e dopo tutti gli altri stati
/// (stesso `Settings`, con le impostazioni lasciate dagli stati precedenti in ogni profilo).
#[test]
fn stati_catturati_dai_controlli() {
    let dirs: Vec<PathBuf> = stati_verify().into_iter().chain(catture_w1()).collect();
    assert!(dirs.len() == 17 || dirs.len() == 36, "catture: {}", dirs.len());
    let mut storia = Settings::default();
    for dir in &dirs {
        let nome = dir.strip_prefix(root()).unwrap().display().to_string();
        let target = stato(dir);
        for s in [&mut Settings::default(), &mut storia] {
            usa_i_controlli(s, &target);
            assert_eq!(s.active(), target, "{nome}");
            file_identici(&nome, &s.active(), dir);
        }
    }
    eprintln!("stati raggiunti con i controlli: {}", dirs.len());
}

/// Gaming con la Modalità performance spenta e IEQ Non attivo dà gli stessi blob della
/// Modalità performance (doc 12.9), come le due catture.
#[test]
fn gaming_performance_spenta_e_ieq_off() {
    let mut s = Settings::default();
    s.select(Kind::Gaming);
    let perf = s.active();
    s.set_performance(false).unwrap();
    s.set_ieq(Ieq::Off).unwrap();
    assert_eq!(s.active(), perf);
    assert!(s.set_performance(true).is_ok() && s.set_ieq(Ieq::Warm).is_err(), "preset nascosti con la modalità accesa");
    if let Some(dir) = stati_verify().into_iter().find(|d| d.to_string_lossy().contains("gaming-perfoff-ieq-off")) {
        file_identici("gaming-perfoff-ieq-off", &perf, &dir);
    }
}

/// Criterio 2: ogni profilo ricorda le sue impostazioni.
#[test]
fn memoria_per_profilo() {
    let mut s = Settings::default();
    s.select(Kind::Film);
    s.set_ieq(Ieq::Warm).unwrap();
    s.select(Kind::Custom1);
    s.set_virtualizer(false).unwrap();
    s.set_geq(0, -145).unwrap();
    s.select(Kind::Gaming);
    s.set_performance(false).unwrap();
    s.set_ieq(Ieq::Detailed).unwrap();
    s.set_performance(true).unwrap();
    s.select(Kind::Music);
    s.select(Kind::Film);
    assert_eq!(s.active(), Profile::Film { ieq: Ieq::Warm });
    s.select(Kind::Custom1);
    let mut c = Custom { virtualizer: false, ..Custom::default() };
    c.geq[0] = -145;
    assert_eq!(s.active(), Profile::Custom { slot: 1, settings: c });
    assert_eq!(s.state(Kind::Custom2), Profile::Custom { slot: 2, settings: Custom::default() });
    s.select(Kind::Gaming);
    assert_eq!(s.active(), Profile::Gaming { ieq: Ieq::Off });
    s.set_performance(false).unwrap();
    assert_eq!(s.active(), Profile::Gaming { ieq: Ieq::Detailed }, "preset ritrovato spegnendo la modalità");
    // La memoria sopravvive al file.
    assert_eq!(Settings::from_json(&s.to_json()).unwrap(), s);
}

/// Criterio 2: "Predefinito" tocca solo il profilo attivo e dà i default catturati in W1
/// (`w1-*-default`, `w1-geq-reset`, partite da stati diversi dal default).
#[test]
fn predefinito() {
    let captures = root().join("docs/reference/captures");
    let mut s = Settings::default();
    // Stati di partenza delle catture W1 (doc 12.14, procedura W1).
    for p in [
        Profile::Gaming { ieq: Ieq::Warm },
        Profile::Film { ieq: Ieq::Warm },
        Profile::Music { ieq: Ieq::Warm },
        Profile::Custom { slot: 1, settings: Custom { virtualizer: false, leveler: true, geq: [50; 10] } },
        Profile::Custom { slot: 2, settings: Custom { virtualizer: true, leveler: false, geq: [-100; 10] } },
    ] {
        usa_i_controlli(&mut s, &p);
    }
    for (kind, cattura) in [
        (Kind::Gaming, "w1-gaming-default"),
        (Kind::Film, "w1-film-default"),
        (Kind::Music, "w1-music-default"),
        (Kind::Custom1, "w1-custom1-default"),
        (Kind::Custom2, "w1-geq-reset"),
    ] {
        s.select(kind);
        let altri: Vec<Profile> = Kind::ALL.iter().filter(|&&k| k != kind).map(|&k| s.state(k)).collect();
        s.reset();
        file_identici(cattura, &s.active(), &captures.join(cattura));
        let dopo: Vec<Profile> = Kind::ALL.iter().filter(|&&k| k != kind).map(|&k| s.state(k)).collect();
        assert_eq!(altri, dopo, "{cattura}: Predefinito ha toccato altri profili");
    }
    s.select(Kind::Voice);
    s.reset();
    assert_eq!(s.active(), Profile::Voice);
}

/// Criterio 2: GEQ limitato a ±12 dB, bande 0-9, controlli assenti nei profili che non li hanno.
#[test]
fn limiti_e_controlli_assenti() {
    let mut s = Settings::default();
    s.select(Kind::Custom3);
    assert_eq!(s.set_geq(0, 500).unwrap(), GEQ_RANGE);
    assert_eq!(s.set_geq(9, -500).unwrap(), -GEQ_RANGE);
    assert_eq!(s.set_geq(5, -145).unwrap(), -145);
    assert!(s.set_geq(10, 0).is_err());
    assert!(s.set_ieq(Ieq::Warm).is_err() && s.set_performance(false).is_err());
    assert_eq!(s.ieq(), None);
    assert_eq!(s.custom().unwrap().geq[0], GEQ_RANGE);
    s.select(Kind::Voice);
    assert!(s.set_ieq(Ieq::Warm).is_err() && s.set_virtualizer(false).is_err() && s.set_geq(0, 0).is_err());
    s.select(Kind::Film);
    assert!(s.set_leveler(true).is_err() && s.set_performance(false).is_err());
    assert_eq!(s.custom(), None);
    assert_eq!(s.ieq(), Some(Ieq::Off));
}

/// Uno stato importato da Windows diventa il profilo attivo, senza toccare gli altri.
#[test]
fn applica_stato_importato() {
    let mut s = Settings::default();
    for dir in catture_w1() {
        let p = stato(&dir);
        let prima = s.clone();
        s.apply(p);
        assert_eq!(s.active(), p);
        for k in Kind::ALL.into_iter().filter(|&k| k != Kind::of(&p)) {
            assert_eq!(s.state(k), prima.state(k));
        }
    }
    s.apply(Profile::Gaming { ieq: Ieq::Balanced });
    assert!(!s.performance());
    s.apply(Profile::Gaming { ieq: Ieq::Off });
    assert!(s.performance());
    assert_eq!(s.ieq(), Some(Ieq::Balanced), "il preset nascosto resta");
}
