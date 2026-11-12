//! Controllo e aggiornamento (doc 12.15, "Aggiornamenti"). L'aggiornamento, nell'ordine:
//!
//! 1. scarica pacchetto, `.sha256` e `.minisig` in `~/.cache/open-surround/`;
//! 2. verifica la firma minisign con la chiave compilata, poi lo SHA-256;
//! 3. estrae solo le voci attese ([`crate::archive`]);
//! 4. controlla il DLL installato ([`crate::dll`]): assente → fermo; sconosciuto → solo con
//!    conferma;
//! 5. copia di sicurezza di `~/.local/bin/os-*`, unità systemd e voce di menu;
//! 6. sostituisce binari, unità e voce di menu con rename atomici;
//! 7. `systemctl --user daemon-reload`, e riavvio di `open-surround` solo se era attivo;
//! 8. aspetta che il nodo risponda a `stato` con la versione nuova; se non succede rimette i
//!    binari di prima e riavvia di nuovo;
//! 9. aggiorna `installed.json`.
//!
//! Solo per le installazioni da una release (`source: "release"`); per le altre c'è solo
//! l'avviso.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::archive::{self, TOOLS};
use crate::dll::{self, DllState};
use crate::github::{self, Http};
use crate::installed::{Installed, PackageVersion};
use crate::paths::{copy_atomic, Paths};
use crate::semver;
use crate::sign;

/// Numero di passi di [`apply`] (per la barra di avanzamento).
pub const STEPS: u32 = 9;

const SERVICE: &str = "open-surround.service";

#[derive(Clone, Debug)]
pub struct Config {
    pub paths: Paths,
    /// URL dell'ultima release nell'API di GitHub.
    pub api_url: String,
    /// Chiave pubblica minisign delle release.
    pub public_key: String,
    pub systemctl: PathBuf,
    /// Socket di controllo del nodo, per aspettarlo dopo il riavvio.
    pub socket: PathBuf,
    /// HTTP in chiaro (solo prove con un server locale).
    pub allow_http: bool,
    /// Attesa massima del nodo dopo il riavvio.
    pub restart_wait: Duration,
}

impl Config {
    /// La configurazione vera: la home dell'utente, GitHub, la chiave compilata.
    pub fn from_env() -> Result<Config> {
        #[allow(unused_mut)]
        let mut c = Config {
            paths: Paths::from_env()?,
            api_url: github::LATEST_URL.into(),
            public_key: sign::RELEASE_PUBLIC_KEY.into(),
            systemctl: "systemctl".into(),
            socket: os_profiles::control::default_socket_path().unwrap_or_else(|_| "/nessun-socket".into()),
            allow_http: false,
            restart_wait: Duration::from_secs(30),
        };
        // Solo nelle build con la feature `prova-locale`, mai nel pacchetto (doc 12.15).
        #[cfg(feature = "prova-locale")]
        {
            let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
            if let Some(u) = var("OS_UPDATE_API_URL") {
                c.allow_http = u.starts_with("http://");
                c.api_url = u;
            }
            if let Some(k) = var("OS_UPDATE_PUBKEY") {
                c.public_key = std::fs::read_to_string(&k).with_context(|| k.clone())?;
            }
            if let Some(s) = var("OS_UPDATE_SYSTEMCTL") {
                c.systemctl = s.into();
            }
            eprintln!("os-update: build di prova (prova-locale): {} , systemctl {}", c.api_url, c.systemctl.display());
        }
        Ok(c)
    }
}

/// Errori che la finestra tratta a parte (codici di uscita di `os-update apply`).
#[derive(Debug)]
pub enum UpdateError {
    /// Il DLL installato non c'è: niente è stato toccato.
    DllMancante(PathBuf),
    /// Il DLL installato ha un hash sconosciuto: serve la conferma.
    DllNonVerificato(String),
    /// Installazione non da una release: solo avviso.
    NonDaRelease(String),
    /// Il nodo nuovo non è partito: rimessi i binari di prima.
    Ripristinato(String),
}

impl UpdateError {
    pub fn exit_code(&self) -> i32 {
        match self {
            UpdateError::DllMancante(_) => 3,
            UpdateError::DllNonVerificato(_) => 4,
            UpdateError::NonDaRelease(_) => 5,
            UpdateError::Ripristinato(_) => 6,
        }
    }
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::DllMancante(p) => write!(f, "{}", dll::missing_message(p)),
            UpdateError::DllNonVerificato(h) => write!(
                f,
                "Il DLL installato ha una versione non verificata (SHA-256 {h}): open-surround è verificato con DAP 7.3.7. \
                 Per aggiornare comunque serve la conferma (os-update apply --dll-non-verificato)."
            ),
            UpdateError::NonDaRelease(s) => write!(f, "Installazione {s}: l'aggiornamento dalla finestra c'è solo per le installazioni da una release ({})", github::RELEASES_PAGE),
            UpdateError::Ripristinato(e) => write!(f, "Il nodo aggiornato non risponde ({e}): rimessa la versione di prima"),
        }
    }
}

impl std::error::Error for UpdateError {}

#[derive(Clone, Debug, Default)]
pub struct ApplyOptions {
    /// Procedere con un DLL dall'hash sconosciuto.
    pub accept_unknown_dll: bool,
    /// Reinstallare anche se la versione non è più recente.
    pub force: bool,
}

/// Risultato del controllo, anche in JSON (`os-update check --json`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckReport {
    /// Versione installata (da `installed.json`, altrimenti quella di questo programma).
    pub installata: String,
    /// `release`, `repo` o `sconosciuta` (nessun `installed.json`).
    pub origine: String,
    pub ultima: String,
    pub disponibile: bool,
    /// Note della release (markdown di GitHub).
    pub note: String,
    pub pagina: String,
    /// Si può aggiornare con `os-update apply` (installazione da una release).
    pub dalla_finestra: bool,
    pub dll: DllState,
}

/// Confronta l'ultima release con l'installazione.
pub fn check(cfg: &Config) -> Result<CheckReport> {
    let installed = Installed::load(&cfg.paths.installed())?;
    let release = Http::new(cfg.allow_http).latest(&cfg.api_url)?;
    let (version, origin) = match &installed {
        Some(i) => (i.version.clone(), i.source.clone()),
        None => (os_profiles::version::VERSION.to_string(), "sconosciuta".to_string()),
    };
    Ok(CheckReport {
        // Una versione installata illeggibile (installed.json scritto a mano, pacchetto senza
        // versione.json) conta come più vecchia.
        disponibile: semver::newer(release.version(), &version).unwrap_or(true),
        ultima: release.version().to_string(),
        installata: version,
        dalla_finestra: installed.as_ref().is_some_and(Installed::is_release),
        origine: origin,
        note: release.notes().to_string(),
        pagina: release.html_url.clone(),
        dll: dll::check(&cfg.paths.dll())?,
    })
}

/// Aggiorna; `progress(passo, testo)` a ogni passo. `Ok(None)`: già aggiornato.
pub fn apply(cfg: &Config, opts: &ApplyOptions, progress: &mut dyn FnMut(u32, &str)) -> Result<Option<String>> {
    let p = &cfg.paths;
    let installed = match Installed::load(&p.installed())? {
        Some(i) if i.is_release() => i,
        Some(i) => return Err(UpdateError::NonDaRelease(format!("dal {}", i.source)).into()),
        None => return Err(UpdateError::NonDaRelease("senza installed.json".into()).into()),
    };
    let http = Http::new(cfg.allow_http);
    let release = http.latest(&cfg.api_url)?;
    let version = release.version().to_string();
    semver::Version::parse(&version)?;
    if !opts.force && !semver::newer(&version, &installed.version).unwrap_or(true) {
        return Ok(None);
    }

    // 1. download
    let name = github::package_name(&version);
    progress(1, &format!("Scarico {name}"));
    let cache = p.cache();
    let dl = cache.join("download");
    std::fs::create_dir_all(&dl).with_context(|| dl.display().to_string())?;
    let fetch = |asset_name: &str, limit: u64| -> Result<Option<Vec<u8>>> {
        let Some(a) = release.asset(asset_name) else { return Ok(None) };
        let data = http.download(a, limit)?;
        std::fs::write(dl.join(asset_name), &data).with_context(|| dl.join(asset_name).display().to_string())?;
        Ok(Some(data))
    };
    let package = fetch(&name, github::MAX_PACKAGE)?.with_context(|| format!("la release {} non ha {name}", release.tag_name))?;
    let sha = fetch(&format!("{name}.sha256"), 4096)?.with_context(|| format!("la release {} non ha {name}.sha256", release.tag_name))?;
    let sig = fetch(&format!("{name}.minisig"), 4096)?;

    // 2. firma, poi SHA-256
    progress(2, "Verifico firma e SHA-256");
    sign::verify(&cfg.public_key, &package, sig.as_deref().map(String::from_utf8_lossy).as_deref(), &name)?;
    sign::verify_sha256(&package, &String::from_utf8_lossy(&sha), &name)?;

    // 3. estrazione
    progress(3, "Estraggo il pacchetto");
    let staging = cache.join("estratto");
    remove_dir(&staging)?;
    archive::extract(&package, &github::package_dir(&version), &staging)?;
    let pkg_version: PackageVersion = match std::fs::read_to_string(staging.join("versione.json")) {
        Ok(s) => serde_json::from_str(&s).context("versione.json del pacchetto")?,
        Err(_) => PackageVersion { version: version.clone(), commit: String::new() },
    };
    if pkg_version.version != version {
        bail!("il pacchetto dice versione {}, la release {version}", pkg_version.version);
    }

    // 4. DLL
    progress(4, "Controllo il DLL installato");
    let dll_state = dll::check(&p.dll())?;
    match &dll_state {
        DllState::Mancante => return Err(UpdateError::DllMancante(p.dll()).into()),
        DllState::NonVerificato { sha256 } if !opts.accept_unknown_dll => return Err(UpdateError::DllNonVerificato(sha256.clone()).into()),
        _ => {}
    }

    // 5. copia di sicurezza
    progress(5, "Copia di sicurezza della versione installata");
    let targets = targets(p, &staging);
    let backup = cache.join("precedente");
    remove_dir(&backup)?;
    std::fs::create_dir_all(&backup)?;
    let mut saved = Vec::new();
    for (i, (_, dst, _)) in targets.iter().enumerate() {
        let copy = backup.join(i.to_string());
        if dst.exists() {
            std::fs::copy(dst, &copy).with_context(|| format!("copia di sicurezza di {}", dst.display()))?;
            saved.push(Some(copy));
        } else {
            saved.push(None);
        }
    }
    let was_active = systemctl(cfg, &["is-active", "--quiet", SERVICE]).unwrap_or(false);

    // 6. sostituzione
    progress(6, "Sostituisco i programmi");
    if let Err(e) = replace(&targets) {
        restore(&targets, &saved);
        return Err(e.context("sostituzione dei programmi (rimessi quelli di prima)"));
    }

    // 7. systemd
    progress(7, if was_active { "Ricarico systemd e riavvio il servizio" } else { "Ricarico systemd (servizio non attivo: non lo riavvio)" });
    systemctl(cfg, &["daemon-reload"])?;
    // 8. il nodo nuovo risponde
    if was_active {
        let started = systemctl(cfg, &["restart", SERVICE]);
        progress(8, "Aspetto il nodo aggiornato");
        let result = match started {
            Ok(true) => wait_node(cfg, &version),
            Ok(false) => Err(anyhow::anyhow!("systemctl restart non riuscito")),
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            restore(&targets, &saved);
            let _ = systemctl(cfg, &["daemon-reload"]);
            let _ = systemctl(cfg, &["restart", SERVICE]);
            return Err(UpdateError::Ripristinato(format!("{e:#}")).into());
        }
    } else {
        progress(8, "Servizio non attivo: niente da aspettare");
    }

    // 9. installed.json
    progress(9, "Aggiorno installed.json");
    Installed { version: version.clone(), commit: pkg_version.commit, source: "release".into(), dll_sha256: dll_state.sha256().map(str::to_string) }.save(&p.installed())?;
    Ok(Some(version))
}

/// (sorgente nella cartella estratta, destinazione, permessi)
fn targets(p: &Paths, staging: &Path) -> Vec<(PathBuf, PathBuf, u32)> {
    let mut t: Vec<_> = TOOLS.iter().map(|n| (staging.join("bin").join(n), p.tool(n), 0o755)).collect();
    t.push((staging.join("share/open-surround.service"), p.unit(), 0o644));
    t.push((staging.join("share/open-surround.desktop"), p.desktop(), 0o644));
    t
}

fn replace(targets: &[(PathBuf, PathBuf, u32)]) -> Result<()> {
    for (src, dst, mode) in targets {
        copy_atomic(src, dst, *mode)?;
    }
    Ok(())
}

/// Rimette i file salvati; toglie quelli che prima non c'erano.
fn restore(targets: &[(PathBuf, PathBuf, u32)], saved: &[Option<PathBuf>]) {
    for ((_, dst, mode), copy) in targets.iter().zip(saved) {
        let r = match copy {
            Some(c) => copy_atomic(c, dst, *mode),
            None => std::fs::remove_file(dst).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }).map_err(Into::into),
        };
        if let Err(e) = r {
            eprintln!("ripristino di {}: {e:#}", dst.display());
        }
    }
}

fn remove_dir(d: &Path) -> Result<()> {
    match std::fs::remove_dir_all(d) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).with_context(|| d.display().to_string()),
        _ => Ok(()),
    }
}

/// `systemctl --user <args>`: Ok(true) se esce con 0.
fn systemctl(cfg: &Config, args: &[&str]) -> Result<bool> {
    let st = Command::new(&cfg.systemctl).arg("--user").args(args).status().with_context(|| format!("{} --user {}", cfg.systemctl.display(), args.join(" ")))?;
    Ok(st.success())
}

/// Aspetta che il nodo risponda a `stato` con la versione `version`.
fn wait_node(cfg: &Config, version: &str) -> Result<()> {
    use os_profiles::control::{field, send, Reply};
    let start = Instant::now();
    let mut last = String::from("nessuna risposta");
    while start.elapsed() < cfg.restart_wait {
        match send(&cfg.socket, "stato") {
            Ok(Reply::Ok(s)) => match field(&s, "versione") {
                Some(v) if v == version => return Ok(()),
                v => last = format!("il nodo risponde con la versione {}", v.unwrap_or("non indicata")),
            },
            Ok(Reply::Error(e)) => last = format!("il nodo risponde con un errore: {e}"),
            Err(e) => last = format!("{e:#}"),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!("{last} dopo {} s", cfg.restart_wait.as_secs())
}
