//! os-ctl: manda un comando al nodo `os-pipewire` sul socket di controllo (doc 12.15, L2) e ne
//! stampa la risposta; esce con errore se il nodo rifiuta o non risponde.
//!
//! Uso:
//!   os-ctl [--socket <path>] <comando…>
//!     stato                                profilo attivo e stato del modulo
//!     profilo <cartella|stato> [frame <n>]  cambio di profilo a caldo
//!   --socket   default $XDG_RUNTIME_DIR/open-surround/control.sock

use anyhow::{bail, Result};
use os_profiles::control::{default_socket_path, send, Reply};

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let socket = match args.first().map(String::as_str) {
        Some("--socket") if args.len() >= 2 => {
            let p = args.remove(1).into();
            args.remove(0);
            p
        }
        Some("-h" | "--help") | None => {
            eprintln!("uso: os-ctl [--socket <path>] stato | profilo <cartella|stato> [frame <n>]");
            std::process::exit(if args.is_empty() { 2 } else { 0 });
        }
        _ => default_socket_path()?,
    };
    if args.is_empty() {
        bail!("manca il comando");
    }
    match send(&socket, &args.join(" "))? {
        Reply::Ok(s) => {
            println!("ok {s}");
            Ok(())
        }
        Reply::Error(e) => bail!("{e}"),
    }
}
