//! Canale di controllo del nodo `os-pipewire` (fase 6, doc 12.15, passo L2), lato client.
//!
//! Un socket Unix a righe, una connessione per comando: il client scrive una riga, il nodo
//! risponde con una riga e chiude. La risposta comincia con `ok` o con `errore`:
//!
//! ```text
//! profilo <cartella|stato> [frame <n>]   →  ok profilo <stato>
//! stato                                  →  ok profilo=<stato> modulo=<fermo|in-moto> cambi=<n> …
//! ```
//!
//! Gli stessi comandi del FIFO (`--control`), che resta per gli script di verifica.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};

/// Socket di default: `$XDG_RUNTIME_DIR/open-surround/control.sock`.
pub fn default_socket_path() -> Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).context("XDG_RUNTIME_DIR non impostata")?;
    Ok(PathBuf::from(dir).join("open-surround").join("control.sock"))
}

/// Tempo massimo per la risposta del nodo.
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// Risposta del nodo a un comando.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// Comando accettato, con il testo che segue `ok`.
    Ok(String),
    /// Comando rifiutato, con il motivo.
    Error(String),
}

impl Reply {
    pub fn parse(line: &str) -> Result<Reply> {
        let line = line.trim_end_matches(['\r', '\n']);
        let (head, rest) = line.split_once(' ').unwrap_or((line, ""));
        match head {
            "ok" => Ok(Reply::Ok(rest.to_string())),
            "errore" => Ok(Reply::Error(rest.to_string())),
            _ => bail!("risposta del nodo non riconosciuta: {line:?}"),
        }
    }

    /// `Ok` con il testo, oppure l'errore del nodo come errore.
    pub fn into_result(self) -> Result<String> {
        match self {
            Reply::Ok(s) => Ok(s),
            Reply::Error(e) => bail!("il nodo ha rifiutato il comando: {e}"),
        }
    }
}

/// Manda una riga al nodo e ne legge la risposta. Un errore di connessione vuol dire, di
/// solito, che il nodo non è in esecuzione.
#[cfg(unix)]
pub fn send(socket: &Path, command: &str) -> Result<Reply> {
    use std::os::unix::net::UnixStream;
    if command.contains('\n') {
        bail!("comando su più righe");
    }
    let mut stream = UnixStream::connect(socket).with_context(|| format!("nodo non raggiungibile su {}", socket.display()))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.write_all(format!("{command}\n").as_bytes())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).context("risposta del nodo")?;
    if line.is_empty() {
        bail!("il nodo ha chiuso senza rispondere");
    }
    Reply::parse(&line)
}

#[cfg(not(unix))]
pub fn send(_socket: &Path, _command: &str) -> Result<Reply> {
    bail!("il nodo esiste solo su Linux")
}

/// Il comando che rende attivo uno stato del modello.
pub fn profile_command(profile: &crate::Profile) -> String {
    format!("profilo {profile}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risposte() {
        assert_eq!(Reply::parse("ok profilo film:warm\n").unwrap(), Reply::Ok("profilo film:warm".into()));
        assert_eq!(Reply::parse("ok").unwrap(), Reply::Ok(String::new()));
        assert_eq!(Reply::parse("errore profilo x: sconosciuto").unwrap(), Reply::Error("profilo x: sconosciuto".into()));
        assert!(Reply::parse("boh").is_err());
        assert!(Reply::Error("x".into()).into_result().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn andata_e_ritorno_su_socket() {
        use std::os::unix::net::UnixListener;
        let path = std::env::temp_dir().join(format!("os-ctl-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&s).read_line(&mut line).unwrap();
            (&s).write_all(format!("ok eco {}", line.trim()).as_bytes()).unwrap();
        });
        assert_eq!(send(&path, "stato").unwrap(), Reply::Ok("eco stato".into()));
        server.join().unwrap();
        assert!(send(&path, "a\nb").is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(send(&path, "stato").is_err(), "nodo assente");
    }
}
