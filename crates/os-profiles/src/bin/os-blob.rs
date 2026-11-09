//! os-blob: legge e verifica i valori del contesto Dolby catturati da Windows (sostituisce
//! `docs/reference/decode_asar.pl`).
//!
//! Uso:
//!   os-blob dump <file.bin>...     stampa i campi (i file hanno l'header del registro e il nome
//!                                  delle catture: <ep>-<pid>.bin, <ep>-<fmtid8>-<pid>.bin)
//!   os-blob diff <a.bin> <b.bin>   stampa solo i campi diversi fra due valori della stessa chiave
//!   os-blob check <cartella>...    andata e ritorno su tutti i .bin delle cartelle (ricorsivo) e
//!                                  sui contesti completi; esce con errore se un file non torna

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use os_dap_core::props::parse_capture_name;
use os_profiles::blob::{strip_header, Decoded};
use os_profiles::check::check_tree;

fn load(path: &Path) -> Result<Decoded> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let (_, key) = parse_capture_name(name).with_context(|| format!("nome non riconosciuto: {name}"))?;
    let data = std::fs::read(path).with_context(|| path.display().to_string())?;
    Decoded::decode(key, strip_header(&data)?).with_context(|| path.display().to_string())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("dump") => {
            for f in &args[1..] {
                let path = Path::new(f);
                let d = load(path)?;
                println!("=== {} ({} byte senza header) ===", path.display(), d.encode().len());
                for (name, value) in d.describe() {
                    println!("  {name}: {value}");
                }
            }
        }
        Some("diff") if args.len() == 3 => {
            let (a, b) = (load(Path::new(&args[1]))?, load(Path::new(&args[2]))?);
            if std::mem::discriminant(&a) != std::mem::discriminant(&b) {
                bail!("i due file non sono valori dello stesso tipo");
            }
            for ((name, va), (_, vb)) in a.describe().into_iter().zip(b.describe()) {
                if va != vb {
                    println!("  {name}: {va}  ->  {vb}");
                }
            }
        }
        Some("check") if args.len() > 1 => {
            let roots: Vec<PathBuf> = args[1..].iter().map(PathBuf::from).collect();
            let r = check_tree(&roots)?;
            for f in &r.failures {
                eprintln!("ERRORE {f}");
            }
            println!(
                "{} file identici dopo andata e ritorno ({} contenuti distinti), {} contesti completi rigenerati, {} errori",
                r.files,
                r.distinct,
                r.contexts,
                r.failures.len()
            );
            if !r.failures.is_empty() {
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("uso: os-blob dump <file.bin>... | diff <a.bin> <b.bin> | check <cartella>...");
            std::process::exit(2);
        }
    }
    Ok(())
}
