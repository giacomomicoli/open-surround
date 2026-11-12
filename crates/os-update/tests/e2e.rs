//! Prova completa di `apply` (doc 12.15, "Aggiornamenti"): un pacchetto firmato con una chiave
//! di prova, servito da un server HTTP locale come se fosse GitHub, installato in una home
//! temporanea con un `systemctl` finto e un nodo finto sul socket di controllo. Non tocca
//! l'installazione vera, il servizio vero né l'audio.
#![cfg(unix)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use os_update::archive::TOOLS;
use os_update::installed::Installed;
use os_update::paths::Paths;
use os_update::{apply, check, ApplyOptions, Config, UpdateError};

const NEW: &str = "0.2.0";

fn dir_name(v: &str) -> String {
    format!("open-surround-{v}-linux-x86_64")
}

/// Il pacchetto come lo fa package.sh, con programmi finti.
fn package(version: &str) -> Vec<u8> {
    let mut b = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    let prefix = dir_name(version);
    let mut add = |path: &str, data: &[u8], mode: u32| {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(mode);
        h.set_cksum();
        b.append_data(&mut h, format!("{prefix}/{path}"), data).unwrap();
    };
    for t in TOOLS {
        add(&format!("bin/{t}"), format!("#!/bin/sh\necho {t} {version}\n").as_bytes(), 0o755);
    }
    add("share/open-surround.service", format!("[Service] # {version}\n").as_bytes(), 0o644);
    add("share/open-surround.desktop", format!("[Desktop Entry] # {version}\n").as_bytes(), 0o644);
    add("versione.json", format!(r#"{{"version":"{version}","commit":"c0ffee{version}"}}"#).as_bytes(), 0o644);
    add("LEGGIMI.txt", b"leggimi", 0o644);
    add("LICENSE-MIT", b"mit", 0o644);
    b.into_inner().unwrap().finish().unwrap()
}

struct Keys {
    public: String,
    secret: minisign::SecretKey,
}

fn keys() -> Keys {
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    Keys { public: kp.pk.to_box().unwrap().into_string(), secret: kp.sk }
}

fn sign(k: &Keys, data: &[u8], name: &str) -> Vec<u8> {
    minisign::sign(None, &k.secret, data, Some(&format!("file:{name}")), None).unwrap().into_string().into_bytes()
}

fn sha256(data: &[u8]) -> String {
    os_update::sign::sha256_hex(data)
}

/// Server HTTP minimo: GET <percorso> → contenuto, 404 altrimenti.
struct Server {
    base: String,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl Server {
    fn start() -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let files: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
        let f = files.clone();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { return };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                }
                let body = f.lock().unwrap().get(&path).cloned();
                let (status, body) = match body {
                    Some(b) => ("200 OK", b),
                    None => ("404 Not Found", b"{\"message\":\"Not Found\"}".to_vec()),
                };
                let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(&body);
            }
        });
        Server { base, files }
    }

    /// Pubblica una release `version` come l'API di GitHub; `sig`: None = senza .minisig.
    fn publish(&self, version: &str, package: &[u8], sig: Option<Vec<u8>>) {
        let name = format!("{}.tar.gz", dir_name(version));
        let mut f = self.files.lock().unwrap();
        let mut assets = vec![(name.clone(), package.to_vec()), (format!("{name}.sha256"), format!("{}  {name}\n", sha256(package)).into_bytes())];
        if let Some(s) = sig {
            assets.push((format!("{name}.minisig"), s));
        }
        let list: Vec<String> = assets.iter().map(|(n, d)| format!(r#"{{"name":"{n}","size":{},"browser_download_url":"{}/dl/{n}"}}"#, d.len(), self.base)).collect();
        let json = format!(r#"{{"tag_name":"v{version}","body":"Note della {version}","html_url":"{}/releases/v{version}","assets":[{}]}}"#, self.base, list.join(","));
        f.insert("/latest".into(), json.into_bytes());
        for (n, d) in assets {
            f.insert(format!("/dl/{n}"), d);
        }
    }
}

/// Nodo finto: risponde a `stato` con la versione in `version`.
struct FakeNode {
    version: Arc<Mutex<String>>,
}

fn fake_node(socket: &Path, version: &str) -> FakeNode {
    let l = UnixListener::bind(socket).unwrap();
    let v = Arc::new(Mutex::new(version.to_string()));
    let vv = v.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(s) = s else { return };
            let mut line = String::new();
            let _ = BufReader::new((&s).take(4096)).read_line(&mut line);
            let reply = format!("ok profilo=film:off modulo=fermo cambi=0 versione={} commit=x\n", vv.lock().unwrap());
            let _ = (&s).write_all(reply.as_bytes());
        }
    });
    FakeNode { version: v }
}

/// Una home temporanea con open-surround 0.1.0 installato da una release (senza os-update,
/// come le installazioni di prima), il DLL (finto: hash sconosciuto) e un systemctl finto che
/// registra le chiamate; `attivo`: il servizio risulta attivo.
struct Home {
    root: PathBuf,
    cfg: Config,
}

impl Home {
    fn new(name: &str, server: &Server, keys: &Keys, attivo: bool) -> Home {
        let root = std::env::temp_dir().join(format!("os-update-e2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let p = Paths::new(root.join("home"));
        std::fs::create_dir_all(p.bin()).unwrap();
        for t in &TOOLS[..4] {
            std::fs::write(p.tool(t), format!("vecchio {t}")).unwrap();
        }
        for f in [p.unit(), p.desktop()] {
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, "vecchio").unwrap();
        }
        let mut dll = b"MZ".to_vec();
        dll.resize(0x200, 7);
        std::fs::create_dir_all(p.data()).unwrap();
        std::fs::write(p.dll(), &dll).unwrap();
        Installed { version: "0.1.0".into(), commit: "vecchio".into(), source: "release".into(), dll_sha256: None }.save(&p.installed()).unwrap();

        let stub = root.join("systemctl");
        let log = root.join("systemctl.log");
        let active = root.join("attivo");
        if attivo {
            std::fs::write(&active, "").unwrap();
        }
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$*\" in *is-active*) [ -e '{}' ];; *) exit 0;; esac\n",
            log.display(),
            active.display()
        );
        std::fs::write(&stub, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let cfg = Config {
            paths: p,
            api_url: format!("{}/latest", server.base),
            public_key: keys.public.clone(),
            systemctl: stub,
            socket: root.join("control.sock"),
            allow_http: true,
            restart_wait: Duration::from_millis(1500),
        };
        Home { root, cfg }
    }

    fn p(&self) -> &Paths {
        &self.cfg.paths
    }

    fn tool(&self, t: &str) -> Option<String> {
        std::fs::read_to_string(self.p().tool(t)).ok()
    }

    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("systemctl.log")).unwrap_or_default().lines().map(str::to_string).collect()
    }

    fn untouched(&self) {
        for t in &TOOLS[..4] {
            assert_eq!(self.tool(t).as_deref(), Some(format!("vecchio {t}").as_str()), "{t} toccato");
        }
        assert_eq!(self.tool("os-update"), None);
        assert_eq!(std::fs::read_to_string(self.p().unit()).unwrap(), "vecchio");
        assert_eq!(Installed::load(&self.p().installed()).unwrap().unwrap().version, "0.1.0");
    }

    fn apply(&self, opts: &ApplyOptions) -> (anyhow::Result<Option<String>>, Vec<u32>) {
        let mut steps = Vec::new();
        let r = apply(&self.cfg, opts, &mut |n, _| steps.push(n));
        (r, steps)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn err_code(r: anyhow::Result<Option<String>>) -> i32 {
    r.unwrap_err().downcast_ref::<UpdateError>().map_or(1, UpdateError::exit_code)
}

const CONFIRM: ApplyOptions = ApplyOptions { accept_unknown_dll: true, force: false };

#[test]
fn aggiornamento_completo() {
    let k = keys();
    let server = Server::start();
    let pkg = package(NEW);
    let name = format!("{}.tar.gz", dir_name(NEW));
    server.publish(NEW, &pkg, Some(sign(&k, &pkg, &name)));
    let h = Home::new("completo", &server, &k, true);
    let node = fake_node(&h.cfg.socket, NEW);

    // check: c'è la 0.2.0, aggiornabile dalla finestra; DLL non verificato
    let r = check(&h.cfg).unwrap();
    assert!(r.disponibile && r.dalla_finestra, "{r:?}");
    assert_eq!((r.installata.as_str(), r.ultima.as_str(), r.note.as_str()), ("0.1.0", NEW, "Note della 0.2.0"));
    assert!(matches!(r.dll, os_update::dll::DllState::NonVerificato { .. }));

    // DLL sconosciuto senza conferma: fermo al passo 4, niente toccato
    let (res, steps) = h.apply(&ApplyOptions::default());
    assert_eq!(err_code(res), 4);
    assert_eq!(steps, [1, 2, 3, 4]);
    h.untouched();
    assert!(h.log().is_empty(), "systemctl chiamato: {:?}", h.log());

    // con la conferma: tutti i passi
    let (res, steps) = h.apply(&CONFIRM);
    assert_eq!(res.unwrap().as_deref(), Some(NEW));
    assert_eq!(steps, (1..=9).collect::<Vec<_>>());
    for t in TOOLS {
        assert_eq!(h.tool(t).unwrap(), format!("#!/bin/sh\necho {t} {NEW}\n"));
    }
    assert_eq!(std::fs::read_to_string(h.p().unit()).unwrap(), "[Service] # 0.2.0\n");
    assert_eq!(std::fs::read_to_string(h.p().desktop()).unwrap(), "[Desktop Entry] # 0.2.0\n");
    let inst = Installed::load(&h.p().installed()).unwrap().unwrap();
    assert_eq!((inst.version.as_str(), inst.commit.as_str(), inst.source.as_str()), (NEW, "c0ffee0.2.0", "release"));
    assert_eq!(inst.dll_sha256.as_deref(), Some(sha256(&std::fs::read(h.p().dll()).unwrap()).as_str()));
    assert_eq!(h.log(), ["--user is-active --quiet open-surround.service", "--user daemon-reload", "--user restart open-surround.service"]);
    // file scaricati e copia di sicurezza in ~/.cache/open-surround
    assert!(h.p().cache().join("download").join(&name).exists());
    assert!(h.p().cache().join("download").join(format!("{name}.minisig")).exists());
    assert_eq!(std::fs::read_to_string(h.p().cache().join("precedente/1")).unwrap(), "vecchio os-ui");
    // niente file temporanei lasciati in ~/.local/bin
    let mut left: Vec<String> = std::fs::read_dir(h.p().bin()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, ["os-blob", "os-ctl", "os-pipewire", "os-ui", "os-update"]);

    // di nuovo: già aggiornato, nessun passo
    let (res, steps) = h.apply(&CONFIRM);
    assert_eq!(res.unwrap(), None);
    assert!(steps.is_empty());
    assert!(!check(&h.cfg).unwrap().disponibile);
    drop(node);
}

#[test]
fn nodo_nuovo_che_non_parte_ripristino() {
    let k = keys();
    let server = Server::start();
    let pkg = package(NEW);
    server.publish(NEW, &pkg, Some(sign(&k, &pkg, &format!("{}.tar.gz", dir_name(NEW)))));
    let h = Home::new("ripristino", &server, &k, true);
    // Il nodo continua a rispondere con la versione vecchia: quello nuovo non è partito.
    let node = fake_node(&h.cfg.socket, "0.1.0");
    let (res, steps) = h.apply(&CONFIRM);
    let e = res.unwrap_err();
    assert_eq!(e.downcast_ref::<UpdateError>().map(UpdateError::exit_code), Some(6), "{e:#}");
    assert!(format!("{e:#}").contains("versione 0.1.0"), "{e:#}");
    assert_eq!(steps, (1..=8).collect::<Vec<_>>());
    h.untouched();
    assert_eq!(
        h.log(),
        [
            "--user is-active --quiet open-surround.service",
            "--user daemon-reload",
            "--user restart open-surround.service",
            "--user daemon-reload",
            "--user restart open-surround.service"
        ]
    );
    *node.version.lock().unwrap() = "boh".into();
}

#[test]
fn servizio_non_attivo_non_si_riavvia() {
    let k = keys();
    let server = Server::start();
    let pkg = package(NEW);
    server.publish(NEW, &pkg, Some(sign(&k, &pkg, &format!("{}.tar.gz", dir_name(NEW)))));
    let h = Home::new("inattivo", &server, &k, false);
    let (res, _) = h.apply(&CONFIRM);
    assert_eq!(res.unwrap().as_deref(), Some(NEW));
    assert_eq!(h.log(), ["--user is-active --quiet open-surround.service", "--user daemon-reload"]);
}

#[test]
fn rifiuti_prima_di_toccare_i_binari() {
    let k = keys();
    let server = Server::start();
    let pkg = package(NEW);
    let name = format!("{}.tar.gz", dir_name(NEW));

    // DLL mancante: fermo, con il messaggio su dove copiarlo
    server.publish(NEW, &pkg, Some(sign(&k, &pkg, &name)));
    let h = Home::new("rifiuti", &server, &k, true);
    std::fs::remove_file(h.p().dll()).unwrap();
    let (res, steps) = h.apply(&CONFIRM);
    let e = res.unwrap_err();
    assert!(e.to_string().contains("Scegli il DLL"), "{e}");
    assert_eq!(e.downcast_ref::<UpdateError>().unwrap().exit_code(), 3);
    assert_eq!(steps, [1, 2, 3, 4]);
    h.untouched();
    let mut dll = b"MZ".to_vec();
    dll.resize(0x200, 7);
    std::fs::write(h.p().dll(), dll).unwrap();

    // firmato da un'altra chiave
    let other = keys();
    server.publish(NEW, &pkg, Some(sign(&other, &pkg, &name)));
    let (res, steps) = h.apply(&CONFIRM);
    assert!(format!("{:#}", res.unwrap_err()).contains("firma non valida"));
    assert_eq!(steps, [1, 2]);
    h.untouched();

    // senza .minisig
    server.publish(NEW, &pkg, None);
    let (res, _) = h.apply(&CONFIRM);
    assert!(format!("{:#}", res.unwrap_err()).contains("firma mancante"));
    h.untouched();

    // la firma vera di un pacchetto, sotto la release di un'altra versione
    let old = package("0.1.5");
    let sig_old = sign(&k, &old, &format!("{}.tar.gz", dir_name("0.1.5")));
    server.publish(NEW, &old, Some(sig_old));
    let (res, _) = h.apply(&CONFIRM);
    assert!(format!("{:#}", res.unwrap_err()).contains("altro file"));
    h.untouched();

    // pacchetto con un link simbolico, firmato davvero
    let mut b = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    let mut hd = tar::Header::new_gnu();
    hd.set_entry_type(tar::EntryType::Symlink);
    hd.set_size(0);
    b.append_link(&mut hd, format!("{}/bin/os-ui", dir_name(NEW)), "/bin/sh").unwrap();
    let evil = b.into_inner().unwrap().finish().unwrap();
    server.publish(NEW, &evil, Some(sign(&k, &evil, &name)));
    let (res, steps) = h.apply(&CONFIRM);
    assert!(format!("{:#}", res.unwrap_err()).contains("link"));
    assert_eq!(steps, [1, 2, 3]);
    h.untouched();

    // installazione dal repository: solo avviso
    Installed { version: "0.1.0".into(), commit: "x".into(), source: "repo".into(), dll_sha256: None }.save(&h.p().installed()).unwrap();
    server.publish(NEW, &pkg, Some(sign(&k, &pkg, &name)));
    let r = check(&h.cfg).unwrap();
    assert!(r.disponibile && !r.dalla_finestra);
    let (res, steps) = h.apply(&CONFIRM);
    assert_eq!(err_code(res), 5);
    assert!(steps.is_empty());
    assert!(h.log().is_empty());
}
