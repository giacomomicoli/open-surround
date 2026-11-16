//! Lo stato della finestra senza Slint: impostazioni, file, colloquio con il nodo (testabile
//! con un nodo finto).

use std::path::PathBuf;
use std::time::Instant;

use os_profiles::control::{self, Reply};
use os_profiles::settings::Settings;
use os_profiles::Profile;

use crate::view::Throttle;

pub struct App {
    pub settings: Settings,
    pub path: PathBuf,
    pub socket: PathBuf,
    /// Ultimo stato accettato dal nodo (None: nodo mai raggiunto, o perso).
    pub sent: Option<Profile>,
    /// Ultimo stato dell'interruttore generale accettato dal nodo (doc 12.16).
    pub sent_dolby: Option<bool>,
    /// Il nodo rispondeva all'ultima interrogazione.
    pub connected: bool,
    pub throttle: Throttle,
    /// Un invio è già programmato.
    pub flush_pending: bool,
    pub error: String,
}

impl App {
    /// Salva il file e manda al nodo lo stato attivo, se è cambiato.
    pub fn flush(&mut self) {
        self.error.clear();
        if let Err(e) = self.settings.save(&self.path) {
            self.error = format!("Impostazioni non salvate: {e:#}");
        }
        if !self.connected {
            return;
        }
        let active = self.settings.active();
        if self.sent != Some(active) {
            self.send(active);
        }
        let dolby = self.settings.dolby();
        if self.connected && self.sent_dolby != Some(dolby) {
            self.send_dolby(dolby);
        }
    }

    /// Manda al nodo l'interruttore generale.
    pub fn send_dolby(&mut self, on: bool) {
        match control::send(&self.socket, &control::dolby_command(on)) {
            Ok(Reply::Ok(_)) => self.sent_dolby = Some(on),
            Ok(Reply::Error(e)) => self.error = format!("Il nodo ha rifiutato «{}»: {e}", control::dolby_command(on)),
            Err(_) => {
                self.connected = false;
                self.sent = None;
                self.sent_dolby = None;
            }
        }
    }

    pub fn send(&mut self, profile: Profile) {
        match control::send(&self.socket, &control::profile_command(&profile)) {
            Ok(Reply::Ok(_)) => self.sent = Some(profile),
            Ok(Reply::Error(e)) => self.error = format!("Il nodo ha rifiutato {profile}: {e}"),
            Err(_) => {
                self.connected = false;
                self.sent = None;
                self.sent_dolby = None;
            }
        }
        self.throttle.sent(Instant::now());
    }

    /// Chiede lo stato al nodo; restituisce il testo da mostrare.
    pub fn poll(&mut self) -> String {
        match control::send(&self.socket, "stato") {
            Ok(Reply::Ok(text)) => {
                let field = |k: &str| control::field(&text, k).unwrap_or("?").to_string();
                let node_profile = field("profilo");
                let first = !self.connected;
                self.connected = true;
                let active = self.settings.active();
                let same = node_profile.parse::<Profile>().ok() == Some(active);
                if same {
                    self.sent = Some(active);
                } else if first {
                    // Il nodo è partito con un altro profilo: vale quello delle impostazioni.
                    self.send(active);
                }
                // Interruttore generale: un nodo senza il campo (prima della 12.16) è sempre acceso.
                let mut node_dolby = control::parse_on_off(&field("dolby")).unwrap_or(true);
                let dolby = self.settings.dolby();
                if node_dolby == dolby {
                    self.sent_dolby = Some(dolby);
                } else if first {
                    self.send_dolby(dolby);
                    if self.sent_dolby == Some(dolby) {
                        node_dolby = dolby;
                    }
                }
                let modulo = if !node_dolby {
                    "elaborazione Dolby spenta (stereo)"
                } else if field("modulo") == "fermo" {
                    "in attesa di audio"
                } else {
                    "in elaborazione"
                };
                let mut mismatch = String::new();
                if !same && !first {
                    mismatch += &format!(" · il nodo usa {node_profile}");
                }
                if node_dolby != dolby && !first {
                    mismatch += if node_dolby { " · nel nodo Dolby è acceso" } else { " · nel nodo Dolby è spento" };
                }
                format!("Nodo collegato · {modulo}{mismatch}")
            }
            Ok(Reply::Error(e)) => {
                self.connected = true;
                format!("Il nodo risponde con un errore: {e}")
            }
            Err(_) => {
                self.connected = false;
                self.sent = None;
                format!("Nodo non in esecuzione ({})", self.socket.display())
            }
        }
    }
}


// Il nodo finto usa un socket Unix: su Windows il nodo non c'è e i test non si compilano.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use os_profiles::model::Ieq;
    use os_profiles::settings::Kind;

    /// Nodo finto: risponde come os-pipewire (`profilo`, `stato`) e registra i comandi; rifiuta
    /// gli stati che contengono "custom3".
    struct FakeNode {
        socket: PathBuf,
        /// Profilo, comandi ricevuti, interruttore generale.
        state: Arc<Mutex<(String, Vec<String>, bool)>>,
    }

    impl FakeNode {
        fn start(name: &str, profile: &str) -> FakeNode {
            let socket = std::env::temp_dir().join(format!("os-ui-{name}-{}.sock", std::process::id()));
            let _ = std::fs::remove_file(&socket);
            let listener = UnixListener::bind(&socket).unwrap();
            let state = Arc::new(Mutex::new((profile.to_string(), Vec::new(), true)));
            let st = state.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(s) = stream else { return };
                    let mut line = String::new();
                    BufReader::new(&s).read_line(&mut line).unwrap();
                    let line = line.trim().to_string();
                    let mut g = st.lock().unwrap();
                    let reply = match line.split_once(' ') {
                        Some(("profilo", p)) if p.contains("custom3") => "errore no".to_string(),
                        Some(("profilo", p)) => {
                            g.0 = p.to_string();
                            format!("ok profilo {p}")
                        }
                        Some(("dolby", v @ ("on" | "off"))) => {
                            g.2 = v == "on";
                            format!("ok dolby {v}")
                        }
                        _ if line == "stato" => format!("ok profilo={} dolby={} modulo=fermo cambi=0", g.0, control::on_off(g.2)),
                        _ => "errore comando".to_string(),
                    };
                    g.1.push(line);
                    (&s).write_all(format!("{reply}\n").as_bytes()).unwrap();
                }
            });
            FakeNode { socket, state }
        }

        fn profile(&self) -> String {
            self.state.lock().unwrap().0.clone()
        }

        /// Comandi `profilo` ricevuti.
        fn changes(&self) -> Vec<String> {
            self.state.lock().unwrap().1.iter().filter(|c| c.starts_with("profilo")).cloned().collect()
        }
    }

    impl Drop for FakeNode {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.socket);
        }
    }

    fn app(name: &str, socket: PathBuf) -> App {
        let path = std::env::temp_dir().join(format!("os-ui-{name}-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        App {
            settings: Settings::default(),
            path,
            socket,
            sent: None,
            sent_dolby: None,
            connected: false,
            throttle: Throttle::new(Duration::from_millis(100)),
            flush_pending: false,
            error: String::new(),
        }
    }

    #[test]
    fn nodo_assente() {
        let mut a = app("assente", std::env::temp_dir().join("os-ui-nessun-nodo.sock"));
        assert!(a.poll().starts_with("Nodo non in esecuzione"));
        assert!(!a.connected);
        a.settings.select(Kind::Music);
        a.flush();
        assert_eq!(Settings::load(&a.path).unwrap().active_kind(), Kind::Music, "salvato anche senza nodo");
        assert!(a.error.is_empty());
        std::fs::remove_file(&a.path).unwrap();
    }

    #[test]
    fn allineamento_e_cambi() {
        let node = FakeNode::start("cambi", "voice");
        let mut a = app("cambi", node.socket.clone());
        // Primo contatto: il nodo usa voice, le impostazioni film:off → gli si manda film:off.
        assert!(a.poll().starts_with("Nodo collegato"));
        assert_eq!(node.profile(), "film:off");
        assert_eq!(a.sent, Some(Profile::Film { ieq: Ieq::Off }));
        // Un cambio dall'interfaccia arriva al nodo ed è salvato; ripetere lo stesso stato no.
        a.settings.set_ieq(Ieq::Warm).unwrap();
        a.flush();
        a.flush();
        assert_eq!(node.changes(), ["profilo film:off", "profilo film:warm"]);
        assert_eq!(Settings::load(&a.path).unwrap().active(), Profile::Film { ieq: Ieq::Warm });
        // Cambiato da fuori (os-ctl) dopo il primo contatto: lo si mostra, senza imporre il nostro.
        node.state.lock().unwrap().0 = "music:warm".into();
        let status = a.poll();
        assert!(status.contains("il nodo usa music:warm"), "{status}");
        assert_eq!(node.profile(), "music:warm");
        // Un rifiuto del nodo diventa un errore da mostrare.
        a.settings.select(Kind::Custom3);
        a.flush();
        assert!(a.error.contains("rifiutato"), "{}", a.error);
        std::fs::remove_file(&a.path).unwrap();
    }

    #[test]
    fn interruttore_generale() {
        let node = FakeNode::start("dolby", "film:off");
        let mut a = app("dolby", node.socket.clone());
        // Salvato spento: al primo contatto il nodo (acceso) viene spento.
        a.settings.set_dolby(false);
        assert!(a.poll().contains("Dolby spenta"));
        assert!(!node.state.lock().unwrap().2);
        assert_eq!(a.sent_dolby, Some(false));
        // Riacceso dalla finestra: un solo comando, salvato nel file.
        a.settings.set_dolby(true);
        a.flush();
        a.flush();
        assert!(node.state.lock().unwrap().2);
        let cmds: Vec<String> = node.state.lock().unwrap().1.iter().filter(|c| c.starts_with("dolby")).cloned().collect();
        assert_eq!(cmds, ["dolby off", "dolby on"]);
        assert!(Settings::load(&a.path).unwrap().dolby());
        // Spento da fuori (os-ctl): lo si mostra senza imporre il nostro.
        node.state.lock().unwrap().2 = false;
        let status = a.poll();
        assert!(status.contains("nel nodo Dolby è spento"), "{status}");
        assert!(!node.state.lock().unwrap().2);
        std::fs::remove_file(&a.path).unwrap();
    }
}
