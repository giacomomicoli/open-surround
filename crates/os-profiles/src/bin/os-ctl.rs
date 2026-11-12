//! os-ctl: manda un comando al nodo `os-pipewire` sul socket di controllo (doc 12.15, L2) e ne
//! stampa la risposta; esce con errore se il nodo rifiuta o non risponde.
//!
//! Uso:
//!   os-ctl [--socket <path>] <comando…>
//!     stato                                profilo attivo, stato del modulo, versione del nodo
//!     profilo <cartella|stato> [frame <n>]  cambio di profilo a caldo
//!     versione                             versione e commit di os-ctl e, se risponde, del nodo
//!   --socket   default $XDG_RUNTIME_DIR/open-surround/control.sock

use anyhow::{bail, Result};
use os_profiles::control::{default_socket_path, field, send, Reply};
use os_profiles::version;

const USAGE: &str = "uso: os-ctl [--socket <path>] stato | profilo <cartella|stato> [frame <n>] | versione";

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let socket = match args.first().map(String::as_str) {
        Some("--socket") if args.len() >= 2 => {
            let p = args.remove(1).into();
            args.remove(0);
            Some(p)
        }
        Some("-h" | "--help") | None => {
            eprintln!("{USAGE}");
            std::process::exit(if args.is_empty() { 2 } else { 0 });
        }
        _ => None,
    };
    if args.is_empty() {
        bail!("manca il comando");
    }
    if args == ["versione"] {
        // Non serve il nodo: la versione di os-ctl c'è sempre, quella del nodo se risponde.
        println!("os-ctl {}", version::describe());
        let node = socket.map(Ok).unwrap_or_else(default_socket_path).and_then(|s| send(&s, "stato"));
        match node {
            Ok(Reply::Ok(s)) => println!("nodo {} (commit {})", field(&s, "versione").unwrap_or("senza versione (build precedente)"), field(&s, "commit").unwrap_or("?")),
            Ok(Reply::Error(e)) => println!("nodo: errore {e}"),
            Err(_) => println!("nodo: non in esecuzione"),
        }
        return Ok(());
    }
    let socket = match socket {
        Some(s) => s,
        None => default_socket_path()?,
    };
    match send(&socket, &args.join(" "))? {
        Reply::Ok(s) => {
            println!("ok {s}");
            Ok(())
        }
        Reply::Error(e) => bail!("{e}"),
    }
}
