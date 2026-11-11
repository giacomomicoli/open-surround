//! Passo L4 della fase 5 (doc 12.14): le 17 catture mirate di W1 (`docs/reference/captures/w1-*`,
//! Windows, 11/11/2026). Per ognuna lo stato atteso è quello di `import.txt`, scritto su Windows
//! da `os-blob import`: il modello deve rigenerare identici byte per byte i sei file
//! dell'endpoint delle cuffie e l'importazione deve restituire quello stato. È la verifica su
//! Linux della funzione GEQ 10 → 20 bande (con il limite ±192) e dei "Predefinito".

use std::path::{Path, PathBuf};

use os_profiles::model::{geq_expand, Custom, Ieq, Profile, GEQ_RANGE};
use os_profiles::DolbyContext;

const ENDPOINT: &str = "32ed16b6";

fn captures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/reference/captures")
}

fn w1_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(captures())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir() && p.file_name().unwrap().to_string_lossy().starts_with("w1-"))
        .collect();
    dirs.sort();
    dirs
}

fn expected(dir: &Path) -> Profile {
    let text = std::fs::read_to_string(dir.join("import.txt")).unwrap();
    let text = text.trim_start_matches('\u{feff}').trim();
    text.parse().unwrap_or_else(|e| panic!("{}: import.txt: {e:#}", dir.display()))
}

fn import(name: &str) -> Profile {
    let dir = captures().join(name);
    Profile::import(&DolbyContext::read_capture(&dir, ENDPOINT).unwrap()).unwrap()
}

#[test]
fn catture_w1_identiche() {
    let dirs = w1_dirs();
    assert_eq!(dirs.len(), 17, "catture w1-*");
    for dir in dirs {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let profile = expected(&dir);
        for (file, bytes) in profile.context().capture_files(ENDPOINT) {
            let captured = std::fs::read(dir.join(&file)).unwrap();
            assert!(captured == bytes, "{name}: {file} generato diverso da quello catturato");
        }
        let ctx = DolbyContext::read_capture(&dir, ENDPOINT).unwrap();
        let imported = Profile::import(&ctx).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert_eq!(imported, profile, "{name}: importazione");
        assert_eq!(profile.to_string().parse::<Profile>().unwrap(), profile, "{name}: forma testuale");
    }
}

/// Le 20 bande del blob runtime cuffie (`,2`) di ogni cattura GEQ sono quelle di [`geq_expand`]:
/// controllo diretto sui valori, oltre all'identità dei byte.
#[test]
fn geq_venti_bande_dalle_catture() {
    let mut n = 0;
    for dir in w1_dirs() {
        let Profile::Custom { settings, .. } = expected(&dir) else { continue };
        let ctx = DolbyContext::read_capture(&dir, ENDPOINT).unwrap();
        for rt in [&ctx.runtime_dahp, &ctx.runtime_dabs] {
            let config = rt.graphic_equalizer_config.as_ref().unwrap();
            let gains: Vec<i32> = config.iter().map(|&(_, g)| g).collect();
            assert_eq!(gains, geq_expand(&settings.geq), "{}", dir.display());
            assert!(gains.iter().all(|g| g.abs() <= GEQ_RANGE));
        }
        n += 1;
    }
    assert_eq!(n, 14, "catture dei Personalizzati");
}

/// Il pulsante "Predefinito" (catture `w1-*-default` e `w1-geq-reset`, partite da stati diversi).
#[test]
fn predefiniti() {
    use Ieq::*;
    let casi = [
        ("w1-gaming-default", Profile::Gaming { ieq: Warm }),
        ("w1-film-default", Profile::Film { ieq: Warm }),
        ("w1-music-default", Profile::Music { ieq: Warm }),
        ("w1-custom1-default", Profile::Custom { slot: 1, settings: Custom { virtualizer: false, leveler: true, geq: [50; 10] } }),
        ("w1-geq-reset", Profile::Custom { slot: 2, settings: Custom { virtualizer: true, leveler: false, geq: [-100; 10] } }),
    ];
    for (name, prima) in casi {
        assert_eq!(prima.factory_default(), import(name), "{name}");
    }
    assert_eq!(Profile::Voice.factory_default(), Profile::Voice);
}
