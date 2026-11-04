//! Disposizione degli ingressi del modulo (bed, bypass, oggetti dinamici), modalità di rendering
//! e configurazione di ConfigureEncoder.

use anyhow::{bail, Result};

/// Il modulo lavora solo a 48 kHz.
pub const SAMPLE_RATE: u32 = 48_000;

/// Ritardo algoritmico del modulo: il picco della risposta a un impulso esce 512 frame dopo
/// (10,67 ms), in ogni profilo, su bed e bypass e con blocchi di 256, 512 o 1024 frame
/// (misurato offline il 4/11/2026, doc sez. 12.13).
pub const MODULE_DELAY_FRAMES: usize = 512;

/// Canali "bed" statici, nell'ordine di AudioObjectType di Windows Spatial Audio.
pub const NUM_BEDS: usize = 17;
pub const NUM_BYPASS: usize = 2;
pub const MAX_OBJECTS: usize = 128;
pub const NUM_ENTRIES: usize = NUM_BEDS + NUM_BYPASS + MAX_OBJECTS; // 147
/// Ingressi statici: i 17 bed seguiti dai 2 oggetti bypass (stereo delle applicazioni normali).
pub const NUM_STATIC: usize = NUM_BEDS + NUM_BYPASS;
pub const BYPASS_L: usize = NUM_BEDS;
pub const BYPASS_R: usize = NUM_BEDS + 1;
pub const OUT_CHANNELS: usize = 2;

/// Modalità di rendering (`cfg[0]` di ConfigureEncoder). Con 0 il modulo esige uscita stereo
/// e il suo log parla di DAHP; con 1 di DABS. Determina anche le chiavi lette:
/// cuffie `{1b4dab55-…},1` (init) e `,2` (runtime), speaker `,4` e `,5`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Headphones = 0,
    Speakers = 1,
}

impl Mode {
    pub fn pids(self) -> (u32, u32) {
        match self {
            Mode::Headphones => (1, 2),
            Mode::Speakers => (4, 5),
        }
    }
}

pub mod bed {
    pub const FL: usize = 0;
    pub const FR: usize = 1;
    pub const FC: usize = 2;
    pub const LFE: usize = 3;
    pub const SL: usize = 4;
    pub const SR: usize = 5;
    pub const BL: usize = 6;
    pub const BR: usize = 7;
    pub const TFL: usize = 8;
    pub const TFR: usize = 9;
    pub const TBL: usize = 10;
    pub const TBR: usize = 11;
}

/// Controlla che `block_frames` sia accettato dal modulo: multiplo di 256 in [256, 2048].
pub fn check_block_frames(block_frames: usize) -> Result<()> {
    if !(256..=2048).contains(&block_frames) || block_frames % 256 != 0 {
        bail!("block_frames deve essere un multiplo di 256 in [256, 2048]");
    }
    Ok(())
}

/// Argomento `cfg[8]` di IDAPModule2::ConfigureEncoder, come lo passa DolbyHrtfEnc.dll.
pub fn encoder_config(mode: Mode, block_frames: usize) -> [u32; 8] {
    [
        mode as u32,          // 0 cuffie (DAHP), 1 speaker integrati (DABS)
        block_frames as u32,  // campioni per chiamata
        OUT_CHANNELS as u32,  // canali di uscita
        0x3,                  // channel mask stereo
        NUM_BEDS as u32,
        NUM_BYPASS as u32,
        MAX_OBJECTS as u32,
        1,                    // granuli di metadati per blocco
    ]
}

/// Mappa i canali di un WAV (ordine WAVEFORMATEXTENSIBLE) sui bed di Windows Spatial Audio.
pub fn channel_map(channels: u16) -> Result<&'static [usize]> {
    use bed::*;
    Ok(match channels {
        1 => &[FC],
        2 => &[FL, FR],
        6 => &[FL, FR, FC, LFE, SL, SR], // 5.1: posteriori come laterali, come Windows
        8 => &[FL, FR, FC, LFE, BL, BR, SL, SR],
        12 => &[FL, FR, FC, LFE, BL, BR, SL, SR, TFL, TFR, TBL, TBR],
        n => bail!("{n} canali non supportati (1, 2, 6, 8, 12)"),
    })
}

/// Instradamento e guadagno d'ingresso, come li applica Windows prima del modulo.
pub struct Routing {
    /// Ingresso statico (0..NUM_STATIC) per ciascun canale del flusso.
    pub map: &'static [usize],
    pub active: [bool; NUM_STATIC],
    /// Guadagno lineare applicato all'ingresso prima del modulo.
    pub gain: f32,
}

impl Routing {
    /// `stereo_bypass`: uno stereo va negli oggetti bypass 17-18, come lo stereo di
    /// un'applicazione normale su Windows (altrimenti bed FL/FR, come un gioco).
    /// `gain_db`: se assente, quello di Windows: −3 dB sugli oggetti statici (giochi),
    /// 0 dB sullo stereo delle applicazioni (bypass).
    pub fn new(channels: u16, stereo_bypass: bool, gain_db: Option<f32>) -> Result<Self> {
        let bypass = stereo_bypass && channels == 2;
        let map: &'static [usize] = if bypass { &[BYPASS_L, BYPASS_R] } else { channel_map(channels)? };
        let mut active = [false; NUM_STATIC];
        for &b in map {
            active[b] = true;
        }
        let gain_db = gain_db.unwrap_or(if bypass { 0.0 } else { -3.0 });
        Ok(Routing { map, active, gain: db_to_gain(gain_db) })
    }
}

/// dB → guadagno lineare, in f32 come nell'host verificato (fase 1).
pub fn db_to_gain(db: f32) -> f32 {
    if db == 0.0 {
        1.0
    } else {
        10f32.powf(db / 20.0)
    }
}
