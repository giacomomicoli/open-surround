//! Modello dei profili di Dolby Access e serializzatore verso il contesto Dolby dell'endpoint
//! (doc sez. 12.14, L2).
//!
//! Un [`Profile`] ha gli stessi controlli dell'app: tipo di profilo, preset IEQ (Gaming, Film,
//! Musica), interruttori ed equalizzatore grafico a 10 bande (Personalizzati). [`Profile::context`]
//! produce i sei valori che Dolby Access scrive; le tabelle sono ricavate dai 19 stati catturati
//! (`target/verify/<stato>/canali71`), e per ognuno il risultato è identico byte per byte.
//! [`Profile::import`] fa l'inverso e accetta solo i contesti che il modello rigenera identici.

use std::fmt;
use std::str::FromStr;

use anyhow::{anyhow, bail, Context as _, Result};
use os_dap_core::{Mode, PropKey};

use crate::blob::{DafmProfile, DapVrInit, DapVrRuntime, DolbyContext, D5e60007};

/// Preset dell'equalizzatore intelligente. Nei blob la Modalità performance di Gaming è
/// identica a [`Ieq::Off`] (doc 10.4): l'app la mostra come interruttore a parte, ma il
/// modello non può distinguerle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ieq {
    Off,
    Balanced,
    Warm,
    Detailed,
}

/// Frequenze centrali delle 20 bande di IEQ e GEQ, come sono scritte nei blob (Hz).
pub const BANDS_HZ: [i32; 20] = [
    47, 141, 234, 328, 469, 656, 844, 1031, 1313, 1688, 2250, 3000, 3750, 4688, 5813, 7125, 9000, 11250, 13875, 19688,
];

/// Curve target dell'IEQ a 20 bande, in 1/16 dB (doc 10.4). Con l'IEQ spento il blob porta
/// comunque la curva Equilibrato.
const IEQ_BALANCED: [i32; 20] =
    [157, 167, 218, 218, 203, 188, 192, 192, 205, 213, 218, 209, 193, 159, 134, 97, 71, 22, -90, -283];
const IEQ_WARM: [i32; 20] =
    [114, 146, 183, 169, 170, 128, 103, 90, 98, 126, 127, 140, 96, 85, 80, 66, 38, -32, -132, -275];
const IEQ_DETAILED: [i32; 20] =
    [150, 142, 188, 216, 189, 195, 202, 199, 210, 225, 230, 236, 235, 235, 214, 165, 112, 49, -24, -217];

impl Ieq {
    pub const ALL: [Ieq; 4] = [Ieq::Off, Ieq::Balanced, Ieq::Warm, Ieq::Detailed];

    fn curve(self) -> &'static [i32; 20] {
        match self {
            Ieq::Off | Ieq::Balanced => &IEQ_BALANCED,
            Ieq::Warm => &IEQ_WARM,
            Ieq::Detailed => &IEQ_DETAILED,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Ieq::Off => "off",
            Ieq::Balanced => "balanced",
            Ieq::Warm => "warm",
            Ieq::Detailed => "detailed",
        }
    }
}

/// Frequenze degli slider del GEQ nell'app (Hz) e banda a 20 di ciascuno (indice in [`BANDS_HZ`]).
pub const GEQ_SLIDERS_HZ: [i32; 10] = [32, 64, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
const GEQ_ANCHORS: [usize; 10] = [0, 1, 2, 3, 4, 7, 10, 13, 16, 18];

/// Espande i 10 slider del GEQ (1/16 dB) nelle 20 bande del blob: le bande ancora prendono il
/// valore dello slider, le intermedie sono interpolate linearmente in frequenza fra le due
/// ancore vicine e l'ultima è estrapolata dalle ultime due, con arrotondamento all'intero più
/// vicino, limitato a ±[`GEQ_RANGE`] (conta solo per l'estrapolazione: le interpolate stanno
/// fra due slider). Ricavata da Person. 1 (doc 12.14, esito di L2); il limite da `w1-geq-b`
/// (W1), dove la 19688 Hz estrapolata varrebbe 290 e Dolby Access scrive 192.
pub fn geq_expand(sliders: &[i32; 10]) -> [i32; 20] {
    let mut out = [0; 20];
    for (band, out) in out.iter_mut().enumerate() {
        // segmento fra due ancore che contiene la banda (l'ultimo per l'estrapolazione)
        let seg = GEQ_ANCHORS.windows(2).position(|w| band <= w[1]).unwrap_or(GEQ_ANCHORS.len() - 2);
        let (a, b) = (GEQ_ANCHORS[seg], GEQ_ANCHORS[seg + 1]);
        let (fa, fb, f) = (BANDS_HZ[a] as f64, BANDS_HZ[b] as f64, BANDS_HZ[band] as f64);
        let (ga, gb) = (sliders[seg] as f64, sliders[seg + 1] as f64);
        *out = ((ga + (gb - ga) * (f - fa) / (fb - fa)).round() as i32).clamp(-GEQ_RANGE, GEQ_RANGE);
    }
    out
}

/// Escursione degli slider del GEQ in 1/16 dB (±12 dB; `CustomGainRange` di `settings.json`).
pub const GEQ_RANGE: i32 = 192;

/// Impostazioni di un profilo Personalizzato.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Custom {
    /// Virtualizzatore surround.
    pub virtualizer: bool,
    /// Livellatore del volume.
    pub leveler: bool,
    /// Slider del GEQ (32 Hz … 16 kHz) in 1/16 dB; l'app va da −12 a +12 dB.
    pub geq: [i32; 10],
}

impl Default for Custom {
    /// Lo stato catturato come "Predefinito" di Person. 2 e 3.
    fn default() -> Self {
        Custom { virtualizer: true, leveler: false, geq: [0; 10] }
    }
}

/// Uno stato dell'interfaccia di Dolby Access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Gaming { ieq: Ieq },
    Film { ieq: Ieq },
    Music { ieq: Ieq },
    Voice,
    /// Person. 1, 2 o 3.
    Custom { slot: u8, settings: Custom },
}

/// Valori di un profilo che differiscono fra i tipi, per una modalità (tabelle ricavate dai 19
/// stati; doc 12.14, esito di L2).
struct Row {
    surround_decoder: bool,
    surround_boost: i32,
    leveler_enable: bool,
    leveler_amount: i32,
    ieq_amount: i32,
    de_enable: bool,
    de_amount: i32,
}

impl Profile {
    /// Valore di `DAFM_PROFILE`.
    pub fn kind_id(&self) -> i32 {
        match self {
            Profile::Gaming { .. } => 1,
            Profile::Film { .. } => 2,
            Profile::Music { .. } => 3,
            Profile::Voice => 4,
            Profile::Custom { slot, .. } => 4 + *slot as i32,
        }
    }

    fn ieq(&self) -> Ieq {
        match self {
            Profile::Gaming { ieq } | Profile::Film { ieq } | Profile::Music { ieq } => *ieq,
            Profile::Voice | Profile::Custom { .. } => Ieq::Off,
        }
    }

    fn virtualizer(&self) -> bool {
        match self {
            Profile::Voice => false,
            Profile::Custom { settings, .. } => settings.virtualizer,
            _ => true,
        }
    }

    fn row(&self, mode: Mode) -> Row {
        use Mode::{Headphones as Hp, Speakers as Sp};
        let custom_leveler = matches!(self, Profile::Custom { settings, .. } if settings.leveler);
        let (sd, sb, le, la, ia, de, da) = match (self, mode) {
            (Profile::Gaming { .. }, Hp) => (true, 0, false, 0, 10, false, 0),
            (Profile::Gaming { .. }, Sp) => (true, 0, false, 0, 10, false, 7),
            (Profile::Film { .. }, Hp) => (true, 96, true, 4, 10, false, 0),
            (Profile::Film { .. }, Sp) => (true, 96, false, 5, 10, true, 5),
            (Profile::Music { .. }, Hp) => (false, 0, true, 0, 8, false, 0),
            (Profile::Music { .. }, Sp) => (false, 0, false, 4, 10, false, 7),
            (Profile::Voice, Hp) => (false, 0, false, 0, 0, false, 0),
            (Profile::Voice, Sp) => (false, 0, false, 0, 10, true, 3),
            (Profile::Custom { .. }, Hp) => (true, 48, custom_leveler, 2, 0, false, 0),
            (Profile::Custom { .. }, Sp) => (true, 48, custom_leveler, 4, 10, true, 9),
        };
        Row {
            surround_decoder: sd,
            surround_boost: sb,
            leveler_enable: le,
            leveler_amount: la,
            ieq_amount: ia,
            de_enable: de,
            de_amount: da,
        }
    }

    /// Blob runtime della modalità (`,2` cuffie, `,5` speaker).
    pub fn runtime(&self, mode: Mode) -> DapVrRuntime {
        let mut p = base_runtime(mode);
        let row = self.row(mode);
        let speakers = mode == Mode::Speakers;
        p.output_mode = Some(match (self.virtualizer(), speakers) {
            (true, false) => 9,
            (true, true) => 11,
            (false, _) => 1,
        });
        if speakers && self.virtualizer() {
            p.mix_matrix = Some(MIX_MATRIX.to_vec());
        }
        p.surround_decoder_enable = Some(row.surround_decoder);
        p.surround_boost = Some(row.surround_boost);
        p.volume_leveler_enable = Some(row.leveler_enable);
        p.volume_leveler_amount = Some(row.leveler_amount);
        p.ieq_enable = Some(self.ieq() != Ieq::Off);
        p.ieq_bands = Some(bands(self.ieq().curve()));
        p.ieq_amount = Some(row.ieq_amount);
        p.de_enable = Some(row.de_enable);
        p.de_amount = Some(row.de_amount);
        match self {
            Profile::Custom { settings, .. } => {
                p.graphic_equalizer_enable = Some(true);
                p.graphic_equalizer_config = Some(bands(&geq_expand(&settings.geq)));
            }
            // Fuori dai Personalizzati il GEQ è spento; in cuffie la configurazione è assente,
            // negli speaker piatta.
            _ => {
                p.graphic_equalizer_enable = Some(false);
                p.graphic_equalizer_config = speakers.then(|| bands(&[0; 20]));
            }
        }
        p
    }

    /// Blob init della modalità (`,1` cuffie, `,4` speaker): uguale per tutti i profili.
    pub fn init(mode: Mode) -> DapVrInit {
        DapVrInit {
            mi_process_disable: Some(mode == Mode::Headphones),
            virtual_bass_process_enable: Some(false),
            mode: Some(0),
            campo_4: None,
            campo_5: None,
            campo_6: None,
            dynamic_speaker_optimization_enable: Some(false),
        }
    }

    /// Il contesto Dolby completo che Dolby Access scrive per questo stato.
    pub fn context(&self) -> DolbyContext {
        DolbyContext {
            init_dahp: Profile::init(Mode::Headphones),
            runtime_dahp: self.runtime(Mode::Headphones),
            init_dabs: Profile::init(Mode::Speakers),
            runtime_dabs: self.runtime(Mode::Speakers),
            d5e60007: D5e60007 { campo_1: None, campo_2: Some(false) },
            profile: DafmProfile { profile: Some(self.kind_id()) },
        }
    }

    /// Importazione: lo stato del modello da cui viene un contesto catturato. Riesce solo se il
    /// modello rigenera il contesto identico; altrimenti l'errore elenca i campi diversi.
    pub fn import(ctx: &DolbyContext) -> Result<Profile> {
        let rt = &ctx.runtime_dahp;
        let ieq = if rt.ieq_enable == Some(true) {
            let bands = rt.ieq_bands.as_deref().unwrap_or_default();
            *Ieq::ALL[1..]
                .iter()
                .find(|i| bands == self::bands(i.curve()).as_slice())
                .ok_or_else(|| anyhow::anyhow!("curva IEQ sconosciuta: {bands:?}"))?
        } else {
            Ieq::Off
        };
        let profile = match ctx.profile.profile {
            Some(1) => Profile::Gaming { ieq },
            Some(2) => Profile::Film { ieq },
            Some(3) => Profile::Music { ieq },
            Some(4) => Profile::Voice,
            Some(k @ 5..=7) => {
                let geq20 = rt.graphic_equalizer_config.clone().unwrap_or_default();
                if geq20.len() != 20 {
                    bail!("GEQ con {} bande invece di 20", geq20.len());
                }
                let settings = Custom {
                    virtualizer: rt.output_mode != Some(1),
                    leveler: rt.volume_leveler_enable == Some(true),
                    geq: GEQ_ANCHORS.map(|b| geq20[b].1),
                };
                Profile::Custom { slot: (k - 4) as u8, settings }
            }
            other => bail!("DAFM_PROFILE sconosciuto: {other:?}"),
        };
        let again = profile.context();
        if &again != ctx {
            let mut diffs = Vec::new();
            for (name, a, b) in [
                ("init DAHP", ctx.init_dahp.describe(), again.init_dahp.describe()),
                ("runtime DAHP", ctx.runtime_dahp.describe(), again.runtime_dahp.describe()),
                ("init DABS", ctx.init_dabs.describe(), again.init_dabs.describe()),
                ("runtime DABS", ctx.runtime_dabs.describe(), again.runtime_dabs.describe()),
                ("d5e60007", ctx.d5e60007.describe(), again.d5e60007.describe()),
                ("DAFM_PROFILE", ctx.profile.describe(), again.profile.describe()),
            ] {
                for ((field, va), (_, vb)) in a.into_iter().zip(b) {
                    if va != vb {
                        diffs.push(format!("{name}.{field}: catturato {va}, dal modello {vb}"));
                    }
                }
            }
            bail!("stato non rappresentabile dal modello ({profile}):\n  {}", diffs.join("\n  "));
        }
        Ok(profile)
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Profile::Gaming { ieq } => write!(f, "gaming:{}", ieq.name()),
            Profile::Film { ieq } => write!(f, "film:{}", ieq.name()),
            Profile::Music { ieq } => write!(f, "music:{}", ieq.name()),
            Profile::Voice => write!(f, "voice"),
            Profile::Custom { slot, settings: s } => {
                let onoff = |b: bool| if b { "on" } else { "off" };
                let geq: Vec<String> = s.geq.iter().map(i32::to_string).collect();
                write!(
                    f,
                    "custom{slot}:virt={}:leveler={}:geq={}",
                    onoff(s.virtualizer),
                    onoff(s.leveler),
                    geq.join(",")
                )
            }
        }
    }
}

/// Forma testuale, la stessa di `Display` (`--profile`, comando `profilo` del nodo):
/// `gaming|film|music:<off|balanced|warm|detailed>` (per Gaming anche `performance`, uguale a
/// `off`), `voice`, `custom1|2|3[:virt=on|off][:leveler=on|off][:geq=<10 interi>]` con il GEQ
/// in 1/16 dB da 32 Hz a 16 kHz; le opzioni assenti dei Personalizzati prendono i valori di
/// [`Custom::default`].
impl FromStr for Profile {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Profile> {
        let mut parts = s.trim().split(':');
        let kind = parts.next().unwrap_or_default();
        let opts: Vec<&str> = parts.collect();
        let ieq = |allow_performance: bool| -> Result<Ieq> {
            match opts.as_slice() {
                ["performance"] if allow_performance => Ok(Ieq::Off),
                [name] => Ieq::ALL
                    .into_iter()
                    .find(|i| i.name() == *name)
                    .with_context(|| format!("{s}: preset IEQ sconosciuto {name} (off|balanced|warm|detailed)")),
                _ => bail!("{s}: serve {kind}:<off|balanced|warm|detailed>"),
            }
        };
        Ok(match kind {
            "gaming" => Profile::Gaming { ieq: ieq(true)? },
            "film" => Profile::Film { ieq: ieq(false)? },
            "music" => Profile::Music { ieq: ieq(false)? },
            "voice" if opts.is_empty() => Profile::Voice,
            "custom1" | "custom2" | "custom3" => {
                let mut c = Custom::default();
                for opt in opts {
                    let onoff = |v: &str| match v {
                        "on" => Ok(true),
                        "off" => Ok(false),
                        _ => bail!("{s}: {opt}: atteso on|off"),
                    };
                    match opt.split_once('=') {
                        Some(("virt", v)) => c.virtualizer = onoff(v)?,
                        Some(("leveler", v)) => c.leveler = onoff(v)?,
                        Some(("geq", v)) => {
                            let g: Vec<i32> = v
                                .split(',')
                                .map(|x| x.trim().parse::<i32>())
                                .collect::<Result<_, _>>()
                                .with_context(|| format!("{s}: geq"))?;
                            c.geq = g.try_into().map_err(|g: Vec<i32>| anyhow!("{s}: geq con {} valori invece di 10", g.len()))?;
                        }
                        _ => bail!("{s}: opzione sconosciuta {opt} (virt=, leveler=, geq=)"),
                    }
                }
                Profile::Custom { slot: kind.as_bytes()[6] - b'0', settings: c }
            }
            _ => bail!("profilo sconosciuto: {s} (gaming|film|music:<ieq>, voice, custom1|2|3[:…])"),
        })
    }
}

/// I sei valori senza header dello stato dato in forma testuale: la conversione che i
/// programmi installano in `BlobSource::resolver` per `--profile`.
pub fn resolve(text: &str) -> Result<Vec<(PropKey, Vec<u8>)>> {
    Ok(text.parse::<Profile>()?.context().encode())
}

/// Matrice di mix della virtualizzazione negli speaker (Q14), uguale in tutti gli stati.
const MIX_MATRIX: [i32; 16] = [16384, 0, 0, 16384, 11583, 11583, 8192, 8192, 16384, 0, 0, 16384, 16384, 0, 0, 16384];

fn bands(gains: &[i32; 20]) -> Vec<(i32, i32)> {
    BANDS_HZ.iter().copied().zip(gains.iter().copied()).collect()
}

/// I campi uguali in tutti gli stati catturati, per modalità; quelli che dipendono dal profilo
/// li scrive [`Profile::runtime`].
fn base_runtime(mode: Mode) -> DapVrRuntime {
    let speakers = mode == Mode::Speakers;
    let angle = if speakers { 10 } else { 5 };
    DapVrRuntime {
        output_mode: None,
        mix_matrix: None,
        pregain: Some(0),
        postgain: Some(0),
        system_gain: Some(0),
        calibration_boost: Some(0),
        process_optimizer_enable: Some(false),
        process_optimizer_config: None,
        surround_decoder_enable: None,
        virtualizer_headphone_late_gain: Some(0),
        virtualizer_front_speaker_angle: Some(angle),
        virtualizer_surround_speaker_angle: Some(angle),
        virtualizer_height_speaker_angle: Some(angle),
        surround_boost: None,
        mi2ieq_steering_enable: Some(false),
        mi2dv_leveler_steering_enable: Some(false),
        mi2dialog_enhancer_steering_enable: Some(false),
        mi2surround_compressor_steering_enable: Some(false),
        volume_leveler_amount: None,
        volume_leveler_in_target: Some(-320),
        volume_leveler_out_target: Some(-320),
        volume_leveler_enable: None,
        volume_modeler_calibration: Some(0),
        volume_modeler_enable: Some(false),
        ieq_bands: None,
        ieq_enable: None,
        ieq_amount: None,
        de_enable: None,
        de_amount: None,
        de_ducking: Some(0),
        volmax_boost: Some(if speakers { 144 } else { 64 }),
        graphic_equalizer_enable: None,
        graphic_equalizer_config: None,
        audio_optimizer_enable: Some(false),
        audio_optimizer_config: None,
        height_filter_mode: Some(0),
        bass_enhancer_enable: Some(false),
        bass_enhancer_boost: Some(if speakers { 0 } else { 192 }),
        bass_enhancer_cutoff_frequency: Some(200),
        bass_enhancer_width: Some(16),
        bass_extraction_enable: Some(false),
        bass_extraction_cutoff_frequency: Some(200),
        regulator_tuning: None,
        regulator_overdrive: Some(0),
        regulator_timbre_preservation: Some(12),
        regulator_relaxation_amount: Some(96),
        regulator_speaker_distortion_enable: Some(false),
        regulator_enable: Some(false),
        channel_count: Some(2),
        stereo_cp_bypass_mode: Some(1),
        stereo_bypass_dap_dll: Some(false),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geq_person1() {
        // Person. 1 (doc 10.4): slider dall'app e le 20 bande scritte da Dolby Access.
        let sliders = [-145, -113, -43, 0, -31, -34, -39, 80, 57, 50];
        let blob = [-145, -113, -43, 0, -31, -32, -33, -34, -35, -37, -39, -2, 34, 80, 74, 67, 57, 54, 50, 42];
        assert_eq!(geq_expand(&sliders), blob);
        assert_eq!(geq_expand(&[0; 10]), [0; 20]);
        // w1-geq-b (W1): la 19688 Hz estrapolata (290) è limitata a 192.
        let sliders = [-117, -83, -26, 99, -52, -40, 155, -131, 75, 173];
        let blob = [-117, -83, -26, 99, -52, -48, -44, -40, 5, 65, 155, 67, -21, -131, -77, -15, 75, 120, 173, 192];
        assert_eq!(geq_expand(&sliders), blob);
    }

    #[test]
    fn forma_testuale() {
        let p: Profile = "custom2:leveler=on:geq=1,2,3,4,5,6,7,8,9,-10".parse().unwrap();
        assert_eq!(
            p,
            Profile::Custom { slot: 2, settings: Custom { virtualizer: true, leveler: true, geq: [1, 2, 3, 4, 5, 6, 7, 8, 9, -10] } }
        );
        assert_eq!(p.to_string().parse::<Profile>().unwrap(), p);
        assert_eq!("gaming:performance".parse::<Profile>().unwrap(), Profile::Gaming { ieq: Ieq::Off });
        assert_eq!("film:warm".parse::<Profile>().unwrap().to_string(), "film:warm");
        assert_eq!("voice".parse::<Profile>().unwrap(), Profile::Voice);
        assert_eq!("custom3".parse::<Profile>().unwrap(), Profile::Custom { slot: 3, settings: Custom::default() });
        for bad in ["film", "music:performance", "voice:off", "custom4", "custom1:geq=1,2", "custom1:virt=forse", "x"] {
            assert!(bad.parse::<Profile>().is_err(), "{bad}");
        }
    }
}
