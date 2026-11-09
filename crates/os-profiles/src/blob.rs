//! I sei valori del contesto Dolby dell'endpoint che Dolby Access scrive nel registro
//! (`...\FxProperties\{45da5c30-...}\User`, doc sez. 12.14), con lettura e scrittura identiche
//! byte per byte.

use std::path::Path;

use anyhow::{bail, Context as _, Result};
use os_dap_core::props::{parse_capture_name, OTHER_FMTIDS, PKEY_DAPVR_FMTID};
use os_dap_core::PropKey;

use crate::wire::{record, Unknown};

/// Header del valore di registro (PROPVARIANT serializzata: VT_BLOB, poi 1), uguale in tutti i
/// valori catturati. Il modulo riceve il blob senza (`--blob-skip 8`).
pub const REG_HEADER: [u8; 8] = [0x41, 0, 0, 0, 1, 0, 0, 0];

/// Toglie l'header del registro.
pub fn strip_header(data: &[u8]) -> Result<&[u8]> {
    match data.strip_prefix(&REG_HEADER) {
        Some(p) => Ok(p),
        None => bail!("header del valore di registro inatteso: {:02x?}", &data[..data.len().min(8)]),
    }
}

/// Aggiunge l'header del registro.
pub fn with_header(payload: &[u8]) -> Vec<u8> {
    let mut v = REG_HEADER.to_vec();
    v.extend_from_slice(payload);
    v
}

record! {
    /// Parametri runtime DAP-VR, `{1b4dab55},2` (cuffie, DAHP) e `,5` (speaker, DABS): 68 campi
    /// nell'ordine del DataContract di Dolby Access (doc sez. 10.3, `asar-blob-decodifica.txt`).
    /// Guadagni in 1/16 dB; le bande sono coppie (Hz, guadagno).
    DapVrRuntime {
        output_mode: i32,
        mix_matrix: Vec<i32>,
        pregain: i32,
        postgain: i32,
        system_gain: i32,
        calibration_boost: i32,
        process_optimizer_enable: bool,
        process_optimizer_config: Vec<(i32, i32)>,
        surround_decoder_enable: bool,
        virtualizer_headphone_late_gain: i32,
        virtualizer_front_speaker_angle: i32,
        virtualizer_surround_speaker_angle: i32,
        virtualizer_height_speaker_angle: i32,
        surround_boost: i32,
        mi2ieq_steering_enable: bool,
        mi2dv_leveler_steering_enable: bool,
        mi2dialog_enhancer_steering_enable: bool,
        mi2surround_compressor_steering_enable: bool,
        volume_leveler_amount: i32,
        volume_leveler_in_target: i32,
        volume_leveler_out_target: i32,
        volume_leveler_enable: bool,
        volume_modeler_calibration: i32,
        volume_modeler_enable: bool,
        ieq_bands: Vec<(i32, i32)>,
        ieq_enable: bool,
        ieq_amount: i32,
        de_enable: bool,
        de_amount: i32,
        de_ducking: i32,
        volmax_boost: i32,
        graphic_equalizer_enable: bool,
        graphic_equalizer_config: Vec<(i32, i32)>,
        audio_optimizer_enable: bool,
        audio_optimizer_config: Vec<(i32, i32)>,
        height_filter_mode: i32,
        bass_enhancer_enable: bool,
        bass_enhancer_boost: i32,
        bass_enhancer_cutoff_frequency: i32,
        bass_enhancer_width: i32,
        bass_extraction_enable: bool,
        bass_extraction_cutoff_frequency: i32,
        regulator_tuning: Vec<(i32, i32)>,
        regulator_overdrive: i32,
        regulator_timbre_preservation: i32,
        regulator_relaxation_amount: i32,
        regulator_speaker_distortion_enable: bool,
        regulator_enable: bool,
        channel_count: i32,
        stereo_cp_bypass_mode: i32,
        stereo_bypass_dap_dll: bool,
        volume_leveler_drc_enable: bool,
        eyes_tracking_enable: bool,
        speaker_virtualizer_mode: i32,
        speaker_virtualizer_start_bin: i32,
        volume_leveler_compressor_enable: bool,
        output_mode_v2_deprecated: i32,
        mi2adaptive_virtualizer_steering_enable: bool,
        virtual_bass_mode: i32,
        virtual_bass_src_freqs: Vec<i32>,
        virtual_bass_overall_gain: i32,
        virtual_bass_slope_gain: i32,
        virtual_bass_subgains: Vec<i32>,
        virtual_bass_mix_freqs: Vec<i32>,
        speaker_virtualizer_rendering_config: Vec<i32>,
        dynamic_speaker_optimization_amount: i32,
        dynamic_speaker_optimization_speaker_interval: Vec<i32>,
        z_distance: i32,
    }
}

record! {
    /// Parametri init-time DAP-VR, `{1b4dab55},1` (DAHP) e `,4` (DABS): 7 campi, uguali in tutte
    /// le catture salvo il primo (vero in `,1`, falso in `,4`). I nomi dei campi 1-3 e 7 sono
    /// quelli di `dap_vr_init_info` nel log del modulo, verificati cambiando un campo alla volta
    /// (doc 12.14, esito di L1). I campi 4-6 sono sempre assenti e il loro tipo non si conosce:
    /// il 4 accetta 4 byte con valore 0 o 1, il 5 e il 6 né un bool né un int32 a 0.
    DapVrInit {
        mi_process_disable: bool,
        virtual_bass_process_enable: bool,
        mode: i32,
        campo_4: Unknown,
        campo_5: Unknown,
        campo_6: Unknown,
        dynamic_speaker_optimization_enable: bool,
    }
}

record! {
    /// `{d5e60007},2`: 2 campi, sempre il primo assente e il secondo falso. Il significato non
    /// si conosce e il modulo non lo legge (nei log dello stub, in modalità cuffie, all'avvio
    /// chiede solo `,1` e `,2`): va generato solo per l'identità del contesto.
    D5e60007 {
        campo_1: Unknown,
        campo_2: bool,
    }
}

record! {
    /// `{e36464a1},1` `DAFM_PROFILE`: tipo di profilo (1 Gaming, 2 Film, 3 Musica, 4 Voce,
    /// 5/6/7 Personalizzato 1/2/3).
    DafmProfile {
        profile: i32,
    }
}

/// Chiavi dei sei valori.
pub const KEY_INIT_DAHP: PropKey = PropKey { fmtid: PKEY_DAPVR_FMTID, pid: 1 };
pub const KEY_RUNTIME_DAHP: PropKey = PropKey { fmtid: PKEY_DAPVR_FMTID, pid: 2 };
pub const KEY_INIT_DABS: PropKey = PropKey { fmtid: PKEY_DAPVR_FMTID, pid: 4 };
pub const KEY_RUNTIME_DABS: PropKey = PropKey { fmtid: PKEY_DAPVR_FMTID, pid: 5 };
pub const KEY_D5E60007: PropKey = PropKey { fmtid: OTHER_FMTIDS[1], pid: 2 };
pub const KEY_DAFM_PROFILE: PropKey = PropKey { fmtid: OTHER_FMTIDS[0], pid: 1 };

/// Un valore decodificato, qualunque sia la chiave.
#[derive(Clone, Debug, PartialEq)]
pub enum Decoded {
    Init(DapVrInit),
    Runtime(DapVrRuntime),
    D5e60007(D5e60007),
    Profile(DafmProfile),
}

impl Decoded {
    /// Decodifica il valore (senza header) della chiave data.
    pub fn decode(key: PropKey, payload: &[u8]) -> Result<Decoded> {
        Ok(match key {
            KEY_INIT_DAHP | KEY_INIT_DABS => Decoded::Init(DapVrInit::decode(payload)?),
            KEY_RUNTIME_DAHP | KEY_RUNTIME_DABS => Decoded::Runtime(DapVrRuntime::decode(payload)?),
            KEY_D5E60007 => Decoded::D5e60007(D5e60007::decode(payload)?),
            KEY_DAFM_PROFILE => Decoded::Profile(DafmProfile::decode(payload)?),
            _ => bail!("chiave {key} sconosciuta"),
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            Decoded::Init(v) => v.encode(),
            Decoded::Runtime(v) => v.encode(),
            Decoded::D5e60007(v) => v.encode(),
            Decoded::Profile(v) => v.encode(),
        }
    }

    pub fn describe(&self) -> Vec<(&'static str, String)> {
        match self {
            Decoded::Init(v) => v.describe(),
            Decoded::Runtime(v) => v.describe(),
            Decoded::D5e60007(v) => v.describe(),
            Decoded::Profile(v) => v.describe(),
        }
    }
}

/// Il contesto Dolby completo di un endpoint: i sei valori che Dolby Access riscrive a ogni
/// cambio di profilo.
#[derive(Clone, Debug, PartialEq)]
pub struct DolbyContext {
    pub init_dahp: DapVrInit,
    pub runtime_dahp: DapVrRuntime,
    pub init_dabs: DapVrInit,
    pub runtime_dabs: DapVrRuntime,
    pub d5e60007: D5e60007,
    pub profile: DafmProfile,
}

impl DolbyContext {
    /// I sei valori senza header, ordinati per chiave (la forma di `BlobSource::fx_values`).
    pub fn encode(&self) -> Vec<(PropKey, Vec<u8>)> {
        let mut v = vec![
            (KEY_INIT_DAHP, self.init_dahp.encode()),
            (KEY_RUNTIME_DAHP, self.runtime_dahp.encode()),
            (KEY_INIT_DABS, self.init_dabs.encode()),
            (KEY_RUNTIME_DABS, self.runtime_dabs.encode()),
            (KEY_D5E60007, self.d5e60007.encode()),
            (KEY_DAFM_PROFILE, self.profile.encode()),
        ];
        v.sort_by_key(|(k, _)| *k);
        v
    }

    /// Ricostruisce il contesto dai sei valori senza header; deve esserci ognuno, una volta.
    pub fn decode(values: &[(PropKey, Vec<u8>)]) -> Result<DolbyContext> {
        let get = |key: PropKey| -> Result<&[u8]> {
            let mut it = values.iter().filter(|(k, _)| *k == key);
            match (it.next(), it.next()) {
                (Some((_, v)), None) => Ok(v),
                (None, _) => bail!("manca il valore {key}"),
                _ => bail!("valore {key} ripetuto"),
            }
        };
        Ok(DolbyContext {
            init_dahp: DapVrInit::decode(get(KEY_INIT_DAHP)?).context("init DAHP")?,
            runtime_dahp: DapVrRuntime::decode(get(KEY_RUNTIME_DAHP)?).context("runtime DAHP")?,
            init_dabs: DapVrInit::decode(get(KEY_INIT_DABS)?).context("init DABS")?,
            runtime_dabs: DapVrRuntime::decode(get(KEY_RUNTIME_DABS)?).context("runtime DABS")?,
            d5e60007: D5e60007::decode(get(KEY_D5E60007)?).context("d5e60007")?,
            profile: DafmProfile::decode(get(KEY_DAFM_PROFILE)?).context("DAFM_PROFILE")?,
        })
    }

    /// Legge il contesto di un endpoint (8 cifre esadecimali) da una cartella di cattura
    /// (`<ep>-<pid>.bin`, `<ep>-<fmtid8>-<pid>.bin`, con l'header del registro).
    pub fn read_capture(dir: &Path, endpoint: &str) -> Result<DolbyContext> {
        let mut values = Vec::new();
        for e in std::fs::read_dir(dir).with_context(|| format!("cartella {}", dir.display()))? {
            let e = e?;
            let Some((ep, key)) = e.file_name().to_str().and_then(parse_capture_name) else { continue };
            if ep == endpoint.to_lowercase() {
                let data = std::fs::read(e.path())?;
                let payload = strip_header(&data).with_context(|| e.path().display().to_string())?;
                values.push((key, payload.to_vec()));
            }
        }
        DolbyContext::decode(&values).with_context(|| format!("{} endpoint {endpoint}", dir.display()))
    }

    /// Nomi dei file di cattura e contenuto con header, come li salva `capture_asar.ps1`.
    pub fn capture_files(&self, endpoint: &str) -> Vec<(String, Vec<u8>)> {
        let ep = endpoint.to_lowercase();
        self.encode()
            .into_iter()
            .map(|(key, payload)| {
                let name = if key.fmtid == PKEY_DAPVR_FMTID {
                    format!("{ep}-{}.bin", key.pid)
                } else {
                    format!("{ep}-{:08x}-{}.bin", key.fmtid.data1(), key.pid)
                };
                (name, with_header(&payload))
            })
            .collect()
    }
}
