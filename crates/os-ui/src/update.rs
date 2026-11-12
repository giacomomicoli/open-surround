//! Aggiornamenti nella finestra (doc 12.15, "Aggiornamenti"): controllo all'avvio al più una
//! volta al giorno (disattivabile) e con "Cerca aggiornamenti", banner con versione e note,
//! "Aggiorna" che esegue `os-update apply` e riapre la finestra, avvisi sul DLL installato.
//!
//! Il lavoro lento (rete, `os-update apply`, scelta del file) gira in altri thread e manda
//! messaggi ([`Msg`]) che la finestra legge con un timer. La logica che decide che cosa
//! mostrare è senza Slint ([`banner`], [`dll_alert`]) e ha i suoi test.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

use os_update::dll::{self, DllState};
use os_update::installed::Installed;
use os_update::paths::Paths;
use os_update::prefs::{self, Prefs};
use os_update::{CheckReport, Config};

pub enum Msg {
    /// Esito del controllo; `manual`: chiesto con il pulsante.
    Checked { result: Result<CheckReport, String>, manual: bool },
    /// Una riga `passo n/9 …` di `os-update apply`.
    Progress(String),
    /// Fine di `os-update apply`: codice di uscita e ultimo errore.
    Done { code: i32, error: String },
    /// DLL scelto dall'utente e copiato (None: scelta annullata).
    DllPicked(Result<Option<DllState>, String>),
}

/// Che cosa mostra il banner.
#[derive(Debug, PartialEq, Eq)]
pub struct Banner {
    pub title: String,
    pub notes: String,
    /// 1 "Aggiorna", 2 link alla pagina della release.
    pub action: i32,
}

/// Il banner per un controllo: `None` se non c'è niente di nuovo. `in_app`: questa finestra è
/// quella installata da una release e `os-update` c'è.
pub fn banner(r: &CheckReport, in_app: bool) -> Option<Banner> {
    if !r.disponibile {
        return None;
    }
    let in_app = in_app && r.dalla_finestra;
    let how = if in_app {
        ""
    } else if r.origine == "repo" {
        " · installazione dal repository: aggiorna il repository e rilancia install.sh"
    } else {
        " · scarica il pacchetto dalla pagina della release"
    };
    Some(Banner {
        title: format!("È disponibile open-surround {} (installata {}){how}", r.ultima, r.installata),
        notes: short_notes(&r.note),
        action: if in_app { 1 } else { 2 },
    })
}

/// Le note della release, senza i titoli markdown e al più 12 righe.
fn short_notes(md: &str) -> String {
    let lines: Vec<&str> = md.lines().map(|l| l.trim_end()).filter(|l| !l.trim().is_empty()).map(|l| l.trim_start_matches('#').trim_start()).collect();
    let mut out = lines.iter().take(12).copied().collect::<Vec<_>>().join("\n");
    if lines.len() > 12 {
        out.push_str("\n…");
    }
    out
}

/// Avviso sul DLL: (livello, testo, chiedere conferma). Livello 0 nessuno, 1 non verificato,
/// 2 mancante. `updating`: l'avviso riguarda un aggiornamento da confermare.
pub fn dll_alert(state: &DllState, dest: &std::path::Path, updating: bool) -> (i32, String, bool) {
    match state {
        DllState::Verificato { .. } => (0, String::new(), false),
        DllState::Mancante => (2, format!("Aggiornamento e nodo bloccati. {}", dll::missing_message(dest)), false),
        DllState::NonVerificato { sha256 } => (
            1,
            format!(
                "Il DLL installato ha una versione non verificata (SHA-256 {}…): open-surround è verificato con DAP 7.3.7.{}",
                &sha256[..16.min(sha256.len())],
                if updating { " Aggiornare comunque?" } else { "" }
            ),
            updating,
        ),
    }
}

pub struct Updater {
    pub paths: Paths,
    pub prefs: Prefs,
    pub report: Option<CheckReport>,
    /// Aggiornamento dalla finestra possibile (finestra installata, os-update presente).
    pub in_app: bool,
    pub busy: bool,
    tx: Sender<Msg>,
    pub rx: Receiver<Msg>,
}

impl Updater {
    pub fn new(paths: Paths) -> Updater {
        let prefs = Prefs::load(&paths.prefs());
        let exe = std::env::current_exe().unwrap_or_default();
        let in_app = paths.is_installed_exe(&exe) && paths.tool("os-update").exists();
        let (tx, rx) = channel();
        Updater { paths, prefs, report: None, in_app, busy: false, tx, rx }
    }

    /// Installata con install.sh (c'è installed.json): gli avvisi sul DLL hanno senso.
    pub fn installed(&self) -> bool {
        matches!(Installed::load(&self.paths.installed()), Ok(Some(_)))
    }

    pub fn dll_state(&self) -> DllState {
        dll::check(&self.paths.dll()).unwrap_or(DllState::Mancante)
    }

    /// Controllo automatico da fare ora.
    pub fn due(&self) -> bool {
        self.prefs.due(prefs::now())
    }

    pub fn set_auto(&mut self, on: bool) {
        self.prefs.automatico = on;
        if let Err(e) = self.prefs.save(&self.paths.prefs()) {
            eprintln!("preferenze degli aggiornamenti: {e:#}");
        }
    }

    pub fn checked(&mut self) {
        self.prefs.ultimo_controllo = prefs::now();
        let _ = self.prefs.save(&self.paths.prefs());
    }

    /// Controllo in un altro thread.
    pub fn start_check(&mut self, manual: bool) {
        let tx = self.tx.clone();
        let paths = self.paths.clone();
        std::thread::spawn(move || {
            let result = Config::from_env().map(|mut c| {
                c.paths = paths;
                c
            });
            let result = result.and_then(|c| os_update::check(&c)).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Checked { result, manual });
        });
    }

    /// `os-update apply` in un processo a parte: i binari si sostituiscono sotto la finestra.
    pub fn start_apply(&mut self, accept_unknown_dll: bool) {
        self.busy = true;
        let tx = self.tx.clone();
        let exe = self.paths.tool("os-update");
        std::thread::spawn(move || {
            let mut cmd = Command::new(&exe);
            cmd.arg("apply").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
            if accept_unknown_dll {
                cmd.arg("--dll-non-verificato");
            }
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Msg::Done { code: 1, error: format!("{}: {e}", exe.display()) });
                    return;
                }
            };
            let mut stderr = child.stderr.take().unwrap();
            let err_thread = std::thread::spawn(move || {
                let mut s = String::new();
                let _ = stderr.read_to_string(&mut s);
                s
            });
            for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
                let _ = tx.send(Msg::Progress(line));
            }
            let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(1);
            let err = err_thread.join().unwrap_or_default();
            let error = err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("").trim_start_matches("errore: ").to_string();
            let _ = tx.send(Msg::Done { code, error });
        });
    }

    /// Scelta del DLL con il portale XDG, poi copia al suo posto.
    pub fn pick_dll(&mut self) {
        let tx = self.tx.clone();
        let dest = self.paths.dll();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new().set_title("Scegli DolbyAudioProcessing.dll").add_filter("DLL di Windows", &["dll", "DLL"]).pick_file();
            let r = match picked {
                None => Ok(None),
                Some(src) => dll::install_copy(&src, &dest).map(Some).map_err(|e| format!("{e:#}")),
            };
            let _ = tx.send(Msg::DllPicked(r));
        });
    }

    /// La finestra installata, per riaprirla dopo l'aggiornamento.
    pub fn installed_ui(&self) -> PathBuf {
        self.paths.tool("os-ui")
    }
}

/// `passo 3/9 Estraggo il pacchetto` → `Passo 3 di 9: Estraggo il pacchetto`.
pub fn progress_text(line: &str) -> String {
    match line.strip_prefix("passo ").and_then(|r| r.split_once(' ')) {
        Some((n, text)) => match n.split_once('/') {
            Some((a, b)) => format!("Passo {a} di {b}: {text}"),
            None => line.to_string(),
        },
        None => line.to_string(),
    }
}

/// Apre un URL nel browser.
pub fn open_url(url: &str) {
    if let Err(e) = Command::new("xdg-open").arg(url).stdin(Stdio::null()).spawn() {
        eprintln!("xdg-open {url}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(disponibile: bool, origine: &str) -> CheckReport {
        CheckReport {
            installata: "0.1.0".into(),
            origine: origine.into(),
            ultima: "0.2.0".into(),
            disponibile,
            note: "## Novità\n\n- aggiornamenti dalla finestra\n".into(),
            pagina: "https://example.invalid".into(),
            dalla_finestra: origine == "release",
            dll: DllState::Mancante,
        }
    }

    #[test]
    fn banner_secondo_l_installazione() {
        assert_eq!(banner(&report(false, "release"), true), None);
        let b = banner(&report(true, "release"), true).unwrap();
        assert_eq!(b.action, 1);
        assert_eq!(b.title, "È disponibile open-surround 0.2.0 (installata 0.1.0)");
        assert_eq!(b.notes, "Novità\n- aggiornamenti dalla finestra");
        // release, ma questa finestra è il pacchetto portatile o una build: solo link
        assert_eq!(banner(&report(true, "release"), false).unwrap().action, 2);
        let b = banner(&report(true, "repo"), true).unwrap();
        assert_eq!(b.action, 2);
        assert!(b.title.contains("repository"));
        assert_eq!(banner(&report(true, "sconosciuta"), true).unwrap().action, 2);
        let long: String = (0..20).map(|i| format!("- {i}\n")).collect();
        assert_eq!(short_notes(&long).lines().count(), 13);
    }

    #[test]
    fn avvisi_sul_dll() {
        let dest = std::path::Path::new("/home/x/.local/share/open-surround/DolbyAudioProcessing.dll");
        assert_eq!(dll_alert(&DllState::Verificato { versione: "DAP 7.3.7".into(), sha256: "0f".into() }, dest, true).0, 0);
        let (lvl, text, confirm) = dll_alert(&DllState::Mancante, dest, true);
        assert_eq!((lvl, confirm), (2, false));
        assert!(text.contains(dest.to_str().unwrap()) && text.contains("Scegli il DLL"), "{text}");
        let unknown = DllState::NonVerificato { sha256: "ab".repeat(32) };
        let (lvl, text, confirm) = dll_alert(&unknown, dest, true);
        assert_eq!((lvl, confirm), (1, true));
        assert!(text.contains("non verificata") && text.ends_with("Aggiornare comunque?"), "{text}");
        assert!(!dll_alert(&unknown, dest, false).2);
    }

    /// `os-update apply` in un processo: righe di avanzamento, codice di uscita, ultimo errore.
    /// Con un os-update finto in una home temporanea.
    #[test]
    fn apply_in_un_processo() {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!("os-ui-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let paths = Paths::new(&home);
        std::fs::create_dir_all(paths.bin()).unwrap();
        let script = paths.tool("os-update");
        let run = |body: &str, accept: bool| -> Vec<String> {
            std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let mut u = Updater::new(paths.clone());
            u.start_apply(accept);
            let mut out = Vec::new();
            loop {
                match u.rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
                    Msg::Progress(l) => out.push(progress_text(&l)),
                    Msg::Done { code, error } => {
                        out.push(format!("fine {code} {error}"));
                        return out;
                    }
                    _ => panic!("messaggio inatteso"),
                }
            }
        };
        assert_eq!(run("echo \"passo 1/9 Scarico\"; echo \"$@\" >&2; echo 'errore: DLL non verificato' >&2; exit 4", false), ["Passo 1 di 9: Scarico", "fine 4 DLL non verificato"]);
        assert_eq!(run("echo \"$@\"; exit 0", true), ["apply --dll-non-verificato", "fine 0 "]);
        // os-update assente: errore con il percorso
        std::fs::remove_file(&script).unwrap();
        let mut u = Updater::new(paths.clone());
        u.start_apply(false);
        match u.rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
            Msg::Done { code: 1, error } => assert!(error.contains("os-update"), "{error}"),
            _ => panic!("atteso un errore"),
        }
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn righe_di_avanzamento() {
        assert_eq!(progress_text("passo 3/9 Estraggo il pacchetto"), "Passo 3 di 9: Estraggo il pacchetto");
        assert_eq!(progress_text("aggiornato a 0.2.0"), "aggiornato a 0.2.0");
    }
}
