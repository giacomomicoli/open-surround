//! Verifica di andata e ritorno su cartelle di catture: ogni file `.bin` riconosciuto deve
//! decodificarsi e ricodificarsi negli stessi byte, e ogni contesto completo (sei valori di uno
//! stesso endpoint in una cartella) deve rigenerare gli stessi file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use os_dap_core::props::parse_capture_name;

use crate::blob::{strip_header, with_header, Decoded, DolbyContext};

#[derive(Debug, Default)]
pub struct Report {
    /// File `.bin` verificati.
    pub files: usize,
    /// Contenuti distinti fra quelli verificati.
    pub distinct: usize,
    /// Contesti completi (cartella, endpoint) verificati.
    pub contexts: usize,
    /// Errori: file o contesto e messaggio.
    pub failures: Vec<String>,
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for e in std::fs::read_dir(dir).with_context(|| format!("cartella {}", dir.display()))? {
        let p = e?.path();
        if p.is_dir() {
            walk(&p, out)?;
        } else if p.extension().is_some_and(|x| x == "bin") {
            out.push(p);
        }
    }
    Ok(())
}

fn check_file(path: &Path) -> Result<Option<Vec<u8>>> {
    let Some((_, key)) = path.file_name().and_then(|n| n.to_str()).and_then(parse_capture_name) else {
        return Ok(None);
    };
    let data = std::fs::read(path)?;
    let decoded = Decoded::decode(key, strip_header(&data)?)?;
    let again = with_header(&decoded.encode());
    if again != data {
        anyhow::bail!("ricodificato diverso ({} byte contro {})", again.len(), data.len());
    }
    Ok(Some(data))
}

/// Verifica tutte le catture sotto le cartelle date (ricorsivamente).
pub fn check_tree(roots: &[PathBuf]) -> Result<Report> {
    let mut files = Vec::new();
    for root in roots {
        walk(root, &mut files)?;
    }
    files.sort();
    let mut report = Report::default();
    let mut contents = BTreeSet::new();
    // (cartella, endpoint) -> numero di valori
    let mut groups: BTreeMap<(PathBuf, String), usize> = BTreeMap::new();
    for path in &files {
        match check_file(path) {
            Ok(Some(data)) => {
                report.files += 1;
                contents.insert(data);
                let (ep, _) = parse_capture_name(path.file_name().unwrap().to_str().unwrap()).unwrap();
                *groups.entry((path.parent().unwrap().to_path_buf(), ep)).or_default() += 1;
            }
            Ok(None) => {}
            Err(e) => report.failures.push(format!("{}: {e:#}", path.display())),
        }
    }
    report.distinct = contents.len();
    for ((dir, ep), n) in groups {
        if n != 6 {
            continue;
        }
        let res = DolbyContext::read_capture(&dir, &ep).and_then(|ctx| {
            for (name, bytes) in ctx.capture_files(&ep) {
                let path = dir.join(&name);
                if std::fs::read(&path)? != bytes {
                    anyhow::bail!("{name} rigenerato diverso");
                }
            }
            Ok(())
        });
        match res {
            Ok(()) => report.contexts += 1,
            Err(e) => report.failures.push(format!("{} ({ep}): {e:#}", dir.display())),
        }
    }
    Ok(report)
}
