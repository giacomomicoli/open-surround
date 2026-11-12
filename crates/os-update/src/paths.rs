//! Dove sta un'installazione fatta da `install.sh`, a partire dalla home (le prove usano una
//! home temporanea).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn new(home: impl Into<PathBuf>) -> Paths {
        Paths { home: home.into() }
    }

    /// Dalla variabile `HOME`.
    pub fn from_env() -> Result<Paths> {
        Ok(Paths::new(std::env::var_os("HOME").filter(|h| !h.is_empty()).context("HOME non impostata")?))
    }

    /// `~/.local/bin`
    pub fn bin(&self) -> PathBuf {
        self.home.join(".local/bin")
    }
    /// `~/.local/share/open-surround`: il DLL e `installed.json`.
    pub fn data(&self) -> PathBuf {
        self.home.join(".local/share/open-surround")
    }
    pub fn dll(&self) -> PathBuf {
        self.data().join("DolbyAudioProcessing.dll")
    }
    pub fn installed(&self) -> PathBuf {
        self.data().join("installed.json")
    }
    pub fn unit(&self) -> PathBuf {
        self.home.join(".config/systemd/user/open-surround.service")
    }
    pub fn desktop(&self) -> PathBuf {
        self.home.join(".local/share/applications/open-surround.desktop")
    }
    /// `~/.cache/open-surround`: download, cartella estratta, copia di sicurezza.
    pub fn cache(&self) -> PathBuf {
        self.home.join(".cache/open-surround")
    }
    /// `~/.config/open-surround/aggiornamenti.json`
    pub fn prefs(&self) -> PathBuf {
        self.home.join(".config/open-surround/aggiornamenti.json")
    }

    /// Il programma installato `name` (`~/.local/bin/<name>`).
    pub fn tool(&self, name: &str) -> PathBuf {
        self.bin().join(name)
    }

    /// `exe` è uno dei programmi installati in `~/.local/bin` (e non una build del repository
    /// o il pacchetto portatile).
    pub fn is_installed_exe(&self, exe: &Path) -> bool {
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        canon(exe).parent() == Some(canon(&self.bin()).as_path())
    }
}

/// Crea un file nuovo (errore se esiste) con i permessi `mode` (su Unix).
pub fn create_new(path: &Path, mode: u32) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, mode);
    #[cfg(not(unix))]
    let _ = mode;
    o.open(path)
}

/// Scrive un file in modo atomico: file temporaneo nella stessa cartella, poi rename.
pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().context("percorso senza cartella")?;
    std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
    let tmp = dir.join(format!(".{}.nuovo", path.file_name().context("percorso senza nome")?.to_string_lossy()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = create_new(&tmp, mode).with_context(|| tmp.display().to_string())?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path).with_context(|| format!("{} → {}", tmp.display(), path.display()))
}

/// Copia `src` su `dst` in modo atomico (vedi [`write_atomic`]).
pub fn copy_atomic(src: &Path, dst: &Path, mode: u32) -> Result<()> {
    let data = std::fs::read(src).with_context(|| src.display().to_string())?;
    write_atomic(dst, &data, mode)
}
