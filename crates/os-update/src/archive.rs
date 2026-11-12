//! Estrazione del pacchetto: solo le voci attese, sotto la cartella del pacchetto
//! (`open-surround-<versione>-linux-x86_64/`). Un percorso assoluto, un `..`, un link simbolico
//! o fisico, un dispositivo o una voce fuori dalla cartella fanno rifiutare tutto il pacchetto;
//! le voci regolari sconosciute si saltano; una voce attesa mancante è un errore. I permessi
//! vengono dalla tabella [`ENTRIES`], non dall'archivio.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context as _, Result};

/// Una voce del pacchetto che si estrae.
pub struct Entry {
    /// Percorso dentro la cartella del pacchetto.
    pub path: &'static str,
    pub mode: u32,
    pub required: bool,
}

const fn e(path: &'static str, mode: u32, required: bool) -> Entry {
    Entry { path, mode, required }
}

/// I programmi che l'installazione mette in `~/.local/bin`.
pub const TOOLS: [&str; 5] = ["os-pipewire", "os-ui", "os-ctl", "os-blob", "os-update"];

/// Le voci estratte (le altre si ignorano).
pub const ENTRIES: [Entry; 11] = [
    e("bin/os-pipewire", 0o755, true),
    e("bin/os-ui", 0o755, true),
    e("bin/os-ctl", 0o755, true),
    e("bin/os-blob", 0o755, true),
    e("bin/os-update", 0o755, true),
    e("share/open-surround.service", 0o644, true),
    e("share/open-surround.desktop", 0o644, true),
    e("versione.json", 0o644, false),
    e("open-surround", 0o755, false),
    e("install.sh", 0o755, false),
    e("LEGGIMI.txt", 0o644, false),
];

/// Dimensione massima di una voce (i binari sono di qualche MB).
const MAX_ENTRY: u64 = 256 << 20;

/// Che cosa fare di una voce dell'archivio.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Da estrarre: indice in [`ENTRIES`].
    Extract(usize),
    /// Cartella, o file regolare non atteso: si salta.
    Skip,
}

/// Il tipo di voce che interessa al filtro.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Link,
    Other,
}

/// Decide su una voce senza toccare il disco (testabile): errore per ogni voce pericolosa.
pub fn classify(raw: &Path, kind: Kind, prefix: &str) -> Result<Verdict> {
    let shown = raw.display();
    let mut parts = Vec::new();
    for c in raw.components() {
        match c {
            Component::Normal(p) => parts.push(p.to_str().with_context(|| format!("{shown}: nome non UTF-8"))?),
            Component::CurDir => {}
            Component::ParentDir => bail!("{shown}: percorso con «..», pacchetto rifiutato"),
            Component::RootDir | Component::Prefix(_) => bail!("{shown}: percorso assoluto, pacchetto rifiutato"),
        }
    }
    match kind {
        Kind::Link => bail!("{shown}: link nel pacchetto, rifiutato"),
        Kind::Other => bail!("{shown}: tipo di voce non ammesso, pacchetto rifiutato"),
        _ => {}
    }
    match parts.split_first() {
        Some((first, _)) if *first == prefix => {}
        None if kind == Kind::Dir => return Ok(Verdict::Skip),
        _ => bail!("{shown}: fuori dalla cartella {prefix}/, pacchetto rifiutato"),
    }
    if kind == Kind::Dir {
        return Ok(Verdict::Skip);
    }
    let rel = parts[1..].join("/");
    Ok(ENTRIES.iter().position(|e| e.path == rel).map_or(Verdict::Skip, Verdict::Extract))
}

/// Estrae le voci attese del tar.gz in `dest` (che viene creata, e deve essere vuota o
/// assente); restituisce i percorsi estratti, relativi alla cartella del pacchetto.
pub fn extract(tar_gz: &[u8], prefix: &str, dest: &Path) -> Result<Vec<String>> {
    std::fs::create_dir_all(dest).with_context(|| dest.display().to_string())?;
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(tar_gz));
    let mut seen = Vec::new();
    for entry in ar.entries().context("archivio illeggibile")? {
        let mut entry = entry.context("archivio illeggibile")?;
        let t = entry.header().entry_type();
        let kind = if t.is_file() {
            Kind::File
        } else if t.is_dir() {
            Kind::Dir
        } else if t.is_symlink() || t.is_hard_link() {
            Kind::Link
        } else if t.is_pax_global_extensions() {
            continue;
        } else {
            Kind::Other
        };
        let raw: PathBuf = entry.path().context("nome di voce illeggibile")?.into_owned();
        let Verdict::Extract(i) = classify(&raw, kind, prefix)? else { continue };
        let spec = &ENTRIES[i];
        if seen.iter().any(|s| s == spec.path) {
            bail!("{}: voce ripetuta, pacchetto rifiutato", raw.display());
        }
        if entry.size() > MAX_ENTRY {
            bail!("{}: troppo grande", raw.display());
        }
        let out = dest.join(spec.path);
        std::fs::create_dir_all(out.parent().unwrap())?;
        let mut f = crate::paths::create_new(&out, spec.mode).with_context(|| out.display().to_string())?;
        let n = std::io::copy(&mut (&mut entry).take(MAX_ENTRY), &mut f).with_context(|| format!("estrazione di {}", raw.display()))?;
        if n != entry.size() {
            bail!("{}: voce troncata", raw.display());
        }
        seen.push(spec.path.to_string());
    }
    if let Some(m) = ENTRIES.iter().find(|e| e.required && !seen.iter().any(|s| s == e.path)) {
        bail!("pacchetto incompleto: manca {prefix}/{}", m.path);
    }
    Ok(seen)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const PREFIX: &str = "open-surround-0.2.0-linux-x86_64";

    /// Un tar.gz con le voci date: (percorso, tipo, contenuto). Le intestazioni si scrivono a
    /// mano perché il crate `tar` rifiuta di creare voci con `..` o assolute.
    pub fn tar_gz(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (path, t, data) in entries {
            let mut h = tar::Header::new_gnu();
            let name = &mut h.as_old_mut().name;
            name[..path.len()].copy_from_slice(path.as_bytes());
            h.set_entry_type(*t);
            h.set_mode(0o777);
            h.set_size(if t.is_file() { data.len() as u64 } else { 0 });
            if t.is_symlink() {
                h.set_link_name("/etc/passwd").unwrap();
            }
            h.set_cksum();
            b.append(&h, if t.is_file() { *data } else { &[][..] }).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap()
    }

    /// Le voci obbligatorie di un pacchetto valido.
    pub type Spec = (String, tar::EntryType, Vec<u8>);

    pub fn valid() -> Vec<Spec> {
        ENTRIES.iter().filter(|e| e.required).map(|e| (format!("{PREFIX}/{}", e.path), tar::EntryType::Regular, format!("contenuto di {}", e.path).into_bytes())).collect()
    }

    fn build(v: &[Spec]) -> Vec<u8> {
        tar_gz(&v.iter().map(|(p, t, d)| (p.as_str(), *t, d.as_slice())).collect::<Vec<_>>())
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("os-update-archive-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn filtro_delle_voci() {
        let p = |s: &str| PathBuf::from(s);
        let f = |s: &str| classify(&p(s), Kind::File, PREFIX);
        assert_eq!(f(&format!("{PREFIX}/bin/os-ui")).unwrap(), Verdict::Extract(1));
        assert_eq!(f(&format!("./{PREFIX}/bin/os-update")).unwrap(), Verdict::Extract(4));
        assert_eq!(f(&format!("{PREFIX}/LICENSE-MIT")).unwrap(), Verdict::Skip, "sconosciuta: saltata");
        assert_eq!(f(&format!("{PREFIX}/bin/altro")).unwrap(), Verdict::Skip);
        assert_eq!(classify(&p(&format!("{PREFIX}/bin")), Kind::Dir, PREFIX).unwrap(), Verdict::Skip);
        assert_eq!(classify(&p("."), Kind::Dir, PREFIX).unwrap(), Verdict::Skip);
        for bad in ["/etc/passwd", &format!("/{PREFIX}/bin/os-ui"), &format!("{PREFIX}/../x"), &format!("{PREFIX}/bin/../../x"), "altro/bin/os-ui", "bin/os-ui"] {
            assert!(f(bad).is_err(), "{bad}");
        }
        assert!(classify(&p(&format!("{PREFIX}/bin/os-ui")), Kind::Link, PREFIX).is_err(), "link");
        assert!(classify(&p(&format!("{PREFIX}/bin/x")), Kind::Other, PREFIX).is_err(), "dispositivo, fifo");
    }

    #[test]
    fn estrazione() {
        let mut v = valid();
        v.push((format!("{PREFIX}/LICENSE-MIT"), tar::EntryType::Regular, b"mit".to_vec()));
        v.insert(0, (format!("{PREFIX}/"), tar::EntryType::Directory, vec![]));
        let d = tmp("ok");
        let got = extract(&build(&v), PREFIX, &d).unwrap();
        assert_eq!(got.len(), 7);
        assert_eq!(std::fs::read_to_string(d.join("bin/os-ui")).unwrap(), "contenuto di bin/os-ui");
        assert!(!d.join("LICENSE-MIT").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(d.join("bin/os-ui")).unwrap().permissions().mode() & 0o777, 0o755);
            assert_eq!(std::fs::metadata(d.join("share/open-surround.service")).unwrap().permissions().mode() & 0o777, 0o644);
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn pacchetti_rifiutati() {
        let cases: Vec<(&str, Spec)> = vec![
            ("..", (format!("{PREFIX}/../evil"), tar::EntryType::Regular, b"x".to_vec())),
            ("assoluto", ("/tmp/evil".into(), tar::EntryType::Regular, b"x".to_vec())),
            ("link", (format!("{PREFIX}/share/x"), tar::EntryType::Symlink, vec![])),
            ("fuori", ("altra-cartella/x".into(), tar::EntryType::Regular, b"x".to_vec())),
            ("fifo", (format!("{PREFIX}/x"), tar::EntryType::Fifo, vec![])),
        ];
        for (name, bad) in cases {
            let mut v = valid();
            v.insert(2, bad);
            let d = tmp(name);
            assert!(extract(&build(&v), PREFIX, &d).is_err(), "{name}");
            let _ = std::fs::remove_dir_all(&d);
        }
        // voce obbligatoria mancante
        let mut v = valid();
        v.retain(|(p, _, _)| !p.ends_with("os-update"));
        let d = tmp("incompleto");
        let e = extract(&build(&v), PREFIX, &d).unwrap_err();
        assert!(e.to_string().contains("bin/os-update"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
