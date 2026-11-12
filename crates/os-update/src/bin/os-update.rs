//! os-update: controlla e installa gli aggiornamenti di open-surround (doc 12.15,
//! "Aggiornamenti").
//!
//! Uso:
//!   os-update check [--json]       confronta l'installazione con l'ultima release su GitHub
//!   os-update apply [opzioni]      scarica, verifica (firma minisign, SHA-256) e installa
//!       --dll-non-verificato       procede anche con un DLL dall'hash sconosciuto
//!       --forza                    reinstalla anche se la versione non è più recente
//!   os-update dll <file>           copia il DLL al suo posto (~/.local/share/open-surround/)
//!   os-update versione
//!
//! `apply` stampa una riga `passo <n>/9 <testo>` per passo. Codici di uscita: 0 fatto (o già
//! aggiornato), 3 DLL mancante, 4 DLL non verificato, 5 installazione non da una release,
//! 6 nodo nuovo non partito (rimessa la versione di prima), 1 altri errori.

use std::io::Write;

use anyhow::{bail, Result};
use os_update::dll::{self, DllState};
use os_update::{apply, check, ApplyOptions, Config, UpdateError};

fn usage() -> ! {
    let usage: Vec<&str> = include_str!("os-update.rs").lines().take_while(|l| l.starts_with("//!")).map(|l| l.trim_start_matches("//!")).collect();
    eprintln!("{}", usage.join("\n"));
    std::process::exit(2)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("errore: {e:#}");
        let code = e.downcast_ref::<UpdateError>().map_or(1, UpdateError::exit_code);
        std::process::exit(code);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["check", rest @ ..] => {
            let json = match rest {
                [] => false,
                ["--json"] => true,
                _ => usage(),
            };
            let r = check(&Config::from_env()?)?;
            if json {
                println!("{}", serde_json::to_string(&r)?);
            } else {
                println!("installata {} ({}), ultima {}", r.installata, r.origine, r.ultima);
                if r.disponibile {
                    println!("aggiornamento disponibile: {}", r.pagina);
                    if r.dalla_finestra {
                        println!("per installarlo: os-update apply (o «Aggiorna» nella finestra)");
                    } else {
                        println!("installazione non da una release: scaricala dalla pagina e reinstalla con install.sh");
                    }
                    if !r.note.is_empty() {
                        println!("\n{}", r.note);
                    }
                } else {
                    println!("già aggiornato");
                }
            }
        }
        ["apply", rest @ ..] => {
            let mut opts = ApplyOptions::default();
            for a in rest {
                match *a {
                    "--dll-non-verificato" => opts.accept_unknown_dll = true,
                    "--forza" => opts.force = true,
                    _ => usage(),
                }
            }
            let cfg = Config::from_env()?;
            let out = apply(&cfg, &opts, &mut |n, text| {
                println!("passo {n}/{} {text}", os_update::apply::STEPS);
                let _ = std::io::stdout().flush();
            })?;
            match out {
                Some(v) => println!("aggiornato a {v}"),
                None => println!("già aggiornato"),
            }
        }
        ["dll", file] => {
            let dest = Config::from_env()?.paths.dll();
            match dll::install_copy(file.as_ref(), &dest)? {
                DllState::Verificato { versione, .. } => println!("copiato in {}: {versione}, verificato", dest.display()),
                DllState::NonVerificato { sha256 } => println!("copiato in {}: versione non verificata (SHA-256 {sha256})", dest.display()),
                DllState::Mancante => bail!("copia non riuscita"),
            }
        }
        ["versione" | "--version"] => println!("os-update {}", os_profiles::version::describe()),
        _ => usage(),
    }
    Ok(())
}
