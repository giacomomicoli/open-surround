//! Criterio della fase 5 (doc 12.14): per ognuno dei 19 stati catturati, il contesto generato
//! dal modello è identico byte per byte a quello scritto da Dolby Access, e l'importazione del
//! contesto catturato restituisce lo stesso stato.

use std::path::PathBuf;

use os_profiles::model::{Custom, Ieq, Profile};
use os_profiles::DolbyContext;

const ENDPOINT: &str = "32ed16b6";

fn verify() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/verify");
    dir.is_dir().then_some(dir)
}

/// GEQ di Person. 1 nelle catture (doc 10.4), in 1/16 dB.
const GEQ_PERSON1: [i32; 10] = [-145, -113, -43, 0, -31, -34, -39, 80, 57, 50];

/// I 19 stati di 12.9: cartella di `target/verify` e stato dell'interfaccia.
fn stati() -> Vec<(&'static str, Profile)> {
    use Ieq::*;
    let p1 = |virtualizer, leveler| Profile::Custom { slot: 1, settings: Custom { virtualizer, leveler, geq: GEQ_PERSON1 } };
    vec![
        ("gaming-performance", Profile::Gaming { ieq: Off }),
        ("gaming-perfoff-ieq-off", Profile::Gaming { ieq: Off }),
        ("gaming-balanced", Profile::Gaming { ieq: Balanced }),
        ("gaming-detailed", Profile::Gaming { ieq: Detailed }),
        ("gaming-warm", Profile::Gaming { ieq: Warm }),
        ("film-balanced", Profile::Film { ieq: Balanced }),
        ("film-detailed", Profile::Film { ieq: Detailed }),
        ("film-warm", Profile::Film { ieq: Warm }),
        ("film-ieq-off", Profile::Film { ieq: Off }),
        ("music-balanced", Profile::Music { ieq: Balanced }),
        ("music-detailed", Profile::Music { ieq: Detailed }),
        ("music-warm", Profile::Music { ieq: Warm }),
        ("music-ieq-off", Profile::Music { ieq: Off }),
        ("voice", Profile::Voice),
        ("custom1-virt-on", p1(true, true)),
        ("custom1-virt-off", p1(false, true)),
        ("custom1-leveler-off", p1(true, false)),
        ("custom2-default", Profile::Custom { slot: 2, settings: Custom::default() }),
        ("custom3-default", Profile::Custom { slot: 3, settings: Custom::default() }),
    ]
}

#[test]
fn diciannove_stati_identici() {
    let Some(verify) = verify() else {
        eprintln!("target/verify assente: test saltato (scaricare l'asset verify-data-2026-11-03)");
        return;
    };
    let stati = stati();
    assert_eq!(stati.len(), 19);
    for (name, profile) in stati {
        let dir = verify.join(name).join("canali71");
        for (file, bytes) in profile.context().capture_files(ENDPOINT) {
            let captured = std::fs::read(dir.join(&file)).unwrap();
            assert!(captured == bytes, "{name}: {file} generato diverso da quello catturato");
        }
        let ctx = DolbyContext::read_capture(&dir, ENDPOINT).unwrap();
        assert_eq!(Profile::import(&ctx).unwrap(), profile, "{name}: importazione");
    }
}

/// Ogni contesto completo di `target/verify` (anche le cartelle di segnale diverse da canali71,
/// i doppioni e le catture dell'audio reale) si importa, e il modello lo rigenera identico.
#[test]
fn tutti_i_contesti_si_importano() {
    let Some(verify) = verify() else { return };
    let mut n = 0;
    let mut stack = vec![verify];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            }
        }
        if dir.join(format!("{ENDPOINT}-5.bin")).is_file() && dir.join(format!("{ENDPOINT}-1.bin")).is_file() {
            let ctx = DolbyContext::read_capture(&dir, ENDPOINT).unwrap();
            if let Err(e) = Profile::import(&ctx) {
                panic!("{}: {e:#}", dir.display());
            }
            n += 1;
        }
    }
    assert!(n >= 78, "contesti importati: {n}");
}
