//! Impostazioni dell'interfaccia (fase 6, doc 12.15, passo L1): la logica di Dolby Access senza
//! grafica.
//!
//! Dolby Access ricorda le impostazioni di ognuno dei sette profili: passare da Film:Caldo a
//! Musica e tornare a Film ritrova Caldo. [`Settings`] tiene il profilo attivo e uno stato per
//! profilo, e offre le azioni dell'interfaccia (scegliere il profilo, il preset IEQ, la Modalità
//! performance, gli interruttori e gli slider dei Personalizzati, "Predefinito"). Lo stato da
//! mandare al modulo è [`Settings::active`], un [`Profile`] del modello della fase 5: i blob
//! restano quelli verificati.
//!
//! Il file delle impostazioni è JSON, con la forma testuale di [`Profile`] per ogni profilo
//! ([`Settings::load`], [`Settings::save`], [`default_path`]).

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::model::{Custom, Ieq, Profile, GEQ_RANGE};

/// I sette profili dell'app, nell'ordine in cui li mostra.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Gaming,
    Film,
    Music,
    Voice,
    Custom1,
    Custom2,
    Custom3,
}

impl Kind {
    pub const ALL: [Kind; 7] = [Kind::Gaming, Kind::Film, Kind::Music, Kind::Voice, Kind::Custom1, Kind::Custom2, Kind::Custom3];

    /// Nome nel file delle impostazioni, uguale al prefisso della forma testuale di [`Profile`].
    pub fn name(self) -> &'static str {
        match self {
            Kind::Gaming => "gaming",
            Kind::Film => "film",
            Kind::Music => "music",
            Kind::Voice => "voice",
            Kind::Custom1 => "custom1",
            Kind::Custom2 => "custom2",
            Kind::Custom3 => "custom3",
        }
    }

    /// Il tipo di uno stato del modello.
    pub fn of(profile: &Profile) -> Kind {
        match profile {
            Profile::Gaming { .. } => Kind::Gaming,
            Profile::Film { .. } => Kind::Film,
            Profile::Music { .. } => Kind::Music,
            Profile::Voice => Kind::Voice,
            Profile::Custom { slot, .. } => match slot {
                1 => Kind::Custom1,
                2 => Kind::Custom2,
                _ => Kind::Custom3,
            },
        }
    }

    fn index(self) -> usize {
        Kind::ALL.iter().position(|&k| k == self).unwrap()
    }

    /// Lo stato del profilo con il pulsante "Predefinito" ([`Profile::factory_default`]).
    fn factory(self) -> Profile {
        let any = match self {
            Kind::Gaming => Profile::Gaming { ieq: Ieq::Off },
            Kind::Film => Profile::Film { ieq: Ieq::Off },
            Kind::Music => Profile::Music { ieq: Ieq::Off },
            Kind::Voice => Profile::Voice,
            Kind::Custom1 => Profile::Custom { slot: 1, settings: Custom::default() },
            Kind::Custom2 => Profile::Custom { slot: 2, settings: Custom::default() },
            Kind::Custom3 => Profile::Custom { slot: 3, settings: Custom::default() },
        };
        any.factory_default()
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Kind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Kind> {
        Kind::ALL
            .into_iter()
            .find(|k| k.name() == s)
            .with_context(|| format!("profilo sconosciuto: {s} (gaming, film, music, voice, custom1-3)"))
    }
}

/// Lo stato dell'interfaccia: il profilo attivo e le impostazioni di ognuno dei sette.
///
/// Per Gaming l'app mostra la Modalità performance come interruttore a parte, sopra i preset
/// IEQ; nei blob è solo l'IEQ spento (doc 10.4), quindi [`Settings`] tiene l'interruttore e il
/// preset scelto con l'interruttore spento, e [`Settings::active`] li riduce a `Ieq::Off`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    active: Kind,
    /// Uno stato per profilo, nell'ordine di [`Kind::ALL`]; per Gaming il preset con la
    /// Modalità performance spenta.
    profiles: [Profile; 7],
    performance: bool,
}

impl Default for Settings {
    /// Tutti i profili al loro "Predefinito"; attivo Film. Lo stato di una prima installazione
    /// di Dolby Access non è stato catturato: è una scelta, non una misura.
    fn default() -> Self {
        Settings { active: Kind::Film, profiles: Kind::ALL.map(Kind::factory), performance: true }
    }
}

impl Settings {
    /// Il profilo attivo.
    pub fn active_kind(&self) -> Kind {
        self.active
    }

    /// Lo stato da mandare al modulo (`--profile`, `profilo <stato>`).
    pub fn active(&self) -> Profile {
        self.state(self.active)
    }

    /// Lo stato che il profilo avrebbe se fosse attivo.
    pub fn state(&self, kind: Kind) -> Profile {
        match self.profiles[kind.index()] {
            Profile::Gaming { .. } if self.performance => Profile::Gaming { ieq: Ieq::Off },
            p => p,
        }
    }

    /// Modalità performance di Gaming.
    pub fn performance(&self) -> bool {
        self.performance
    }

    /// Preset IEQ del profilo attivo come lo mostra l'app (per Gaming quello scelto con la
    /// Modalità performance spenta); `None` per Voce e i Personalizzati.
    pub fn ieq(&self) -> Option<Ieq> {
        match self.profiles[self.active.index()] {
            Profile::Gaming { ieq } | Profile::Film { ieq } | Profile::Music { ieq } => Some(ieq),
            _ => None,
        }
    }

    /// Impostazioni del Personalizzato attivo; `None` per gli altri profili.
    pub fn custom(&self) -> Option<Custom> {
        match self.profiles[self.active.index()] {
            Profile::Custom { settings, .. } => Some(settings),
            _ => None,
        }
    }

    /// Sceglie il profilo, con le impostazioni che aveva l'ultima volta.
    pub fn select(&mut self, kind: Kind) {
        self.active = kind;
    }

    /// Preset IEQ di Gaming, Film o Musica. Per Gaming vale con la Modalità performance spenta
    /// (l'app non mostra i preset con la modalità accesa).
    pub fn set_ieq(&mut self, ieq: Ieq) -> Result<()> {
        match &mut self.profiles[self.active.index()] {
            Profile::Gaming { .. } if self.performance => bail!("gaming: preset IEQ con la Modalità performance accesa"),
            Profile::Gaming { ieq: i } | Profile::Film { ieq: i } | Profile::Music { ieq: i } => *i = ieq,
            _ => bail!("{}: nessun preset IEQ", self.active),
        }
        Ok(())
    }

    /// Modalità performance (solo Gaming).
    pub fn set_performance(&mut self, on: bool) -> Result<()> {
        if self.active != Kind::Gaming {
            bail!("{}: la Modalità performance è solo di gaming", self.active);
        }
        self.performance = on;
        Ok(())
    }

    fn custom_mut(&mut self) -> Result<&mut Custom> {
        match &mut self.profiles[self.active.index()] {
            Profile::Custom { settings, .. } => Ok(settings),
            _ => bail!("{}: non è un Personalizzato", self.active),
        }
    }

    /// Virtualizzatore surround (Personalizzati).
    pub fn set_virtualizer(&mut self, on: bool) -> Result<()> {
        self.custom_mut()?.virtualizer = on;
        Ok(())
    }

    /// Livellatore del volume (Personalizzati).
    pub fn set_leveler(&mut self, on: bool) -> Result<()> {
        self.custom_mut()?.leveler = on;
        Ok(())
    }

    /// Slider `band` (0 = 32 Hz … 9 = 16 kHz) del GEQ in 1/16 dB, limitato a ±12 dB come
    /// nell'app. Restituisce il valore impostato.
    pub fn set_geq(&mut self, band: usize, value: i32) -> Result<i32> {
        let c = self.custom_mut()?;
        let slot = c.geq.get_mut(band).with_context(|| format!("banda del GEQ {band} (0-9)"))?;
        *slot = value.clamp(-GEQ_RANGE, GEQ_RANGE);
        Ok(*slot)
    }

    /// Il pulsante "Predefinito": riporta il profilo attivo, e solo quello, al suo default.
    /// Per Gaming riaccende la Modalità performance e lascia il preset nascosto com'era (non si
    /// vede nei blob: le catture non dicono che cosa fa l'app).
    pub fn reset(&mut self) {
        if self.active == Kind::Gaming {
            self.performance = true;
        } else {
            self.profiles[self.active.index()] = self.active.factory();
        }
    }

    /// Rende attivo uno stato del modello, per esempio importato da una cattura di Windows.
    /// `gaming:off` accende la Modalità performance (nei blob sono la stessa cosa).
    pub fn apply(&mut self, profile: Profile) {
        let kind = Kind::of(&profile);
        self.active = kind;
        match profile {
            Profile::Gaming { ieq: Ieq::Off } => self.performance = true,
            Profile::Gaming { .. } => {
                self.performance = false;
                self.profiles[kind.index()] = profile;
            }
            _ => self.profiles[kind.index()] = profile,
        }
    }

    /// Legge il file delle impostazioni; se non esiste, i default.
    pub fn load(path: &Path) -> Result<Settings> {
        match std::fs::read_to_string(path) {
            Ok(text) => Settings::from_json(&text).with_context(|| format!("{}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e).with_context(|| format!("lettura di {}", path.display())),
        }
    }

    /// Scrive il file delle impostazioni (creando la cartella), sostituendolo in un colpo solo:
    /// chi lo legge nello stesso momento (il nodo) vede il vecchio o il nuovo, mai metà.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).with_context(|| format!("creazione di {}", dir.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.to_json()).with_context(|| format!("scrittura di {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("{} → {}", tmp.display(), path.display()))
    }

    pub fn to_json(&self) -> String {
        let file = File {
            version: VERSION,
            active: self.active.name().into(),
            gaming_performance: self.performance,
            profiles: self.profiles.iter().map(Profile::to_string).collect(),
        };
        serde_json::to_string_pretty(&file).unwrap() + "\n"
    }

    pub fn from_json(text: &str) -> Result<Settings> {
        let file: File = serde_json::from_str(text).context("JSON delle impostazioni")?;
        if file.version != VERSION {
            bail!("versione {} delle impostazioni, attesa {VERSION}", file.version);
        }
        let mut s = Settings { active: file.active.parse()?, performance: file.gaming_performance, ..Settings::default() };
        let mut seen = [false; 7];
        for text in &file.profiles {
            let p: Profile = text.parse()?;
            let i = Kind::of(&p).index();
            if std::mem::replace(&mut seen[i], true) {
                bail!("profilo ripetuto: {text}");
            }
            s.profiles[i] = p;
        }
        Ok(s)
    }
}

const VERSION: u32 = 1;

/// Il file su disco. I profili mancanti prendono il "Predefinito".
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    active: String,
    gaming_performance: bool,
    profiles: Vec<String>,
}

/// Percorso del file delle impostazioni: `$XDG_CONFIG_HOME/open-surround/settings.json`
/// (`~/.config/…` se la variabile manca) su Linux, `%APPDATA%\open-surround\settings.json` su
/// Windows.
pub fn default_path() -> Result<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        var("APPDATA").context("APPDATA non impostata")?
    } else {
        match var("XDG_CONFIG_HOME") {
            Some(d) => d,
            None => var("HOME").context("né XDG_CONFIG_HOME né HOME impostate")?.join(".config"),
        }
    };
    Ok(base.join("open-surround").join("settings.json"))
}

/// Valore di uno slider del GEQ in dB, per mostrarlo (1/16 dB → dB).
pub fn geq_db(value: i32) -> f64 {
    value as f64 / 16.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_andata_e_ritorno() {
        let mut s = Settings::default();
        s.select(Kind::Gaming);
        s.set_performance(false).unwrap();
        s.set_ieq(Ieq::Warm).unwrap();
        s.select(Kind::Custom2);
        s.set_geq(3, -50).unwrap();
        let text = s.to_json();
        assert_eq!(Settings::from_json(&text).unwrap(), s);
        // Profili mancanti → default; profili ripetuti, campi sconosciuti, versione → errore.
        let min = r#"{"version":1,"active":"music","gaming_performance":true,"profiles":["music:warm"]}"#;
        let m = Settings::from_json(min).unwrap();
        assert_eq!(m.active(), Profile::Music { ieq: Ieq::Warm });
        assert_eq!(m.state(Kind::Film), Profile::Film { ieq: Ieq::Off });
        for bad in [
            r#"{"version":1,"active":"music","gaming_performance":true,"profiles":["music:warm","music:off"]}"#,
            r#"{"version":1,"active":"music","gaming_performance":true,"profiles":[],"x":1}"#,
            r#"{"version":2,"active":"music","gaming_performance":true,"profiles":[]}"#,
            r#"{"version":1,"active":"custom4","gaming_performance":true,"profiles":[]}"#,
        ] {
            assert!(Settings::from_json(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn file_su_disco() {
        let dir = std::env::temp_dir().join(format!("os-settings-{}", std::process::id()));
        let path = dir.join("sub").join("settings.json");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        let mut s = Settings::default();
        s.select(Kind::Voice);
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn geq_in_db() {
        assert_eq!(geq_db(-145), -9.0625);
        assert_eq!(geq_db(GEQ_RANGE), 12.0);
    }
}
