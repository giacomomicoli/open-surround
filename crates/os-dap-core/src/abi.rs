//! Strutture in memoria lette da IDAPModule2::EncodeAudioData (doc sez. 12.8). Il layout è
//! quello del codice x86-64 del modulo, identico su Windows e Linux.

use crate::layout::NUM_BEDS;

/// Metadati di un oggetto per un granulo (0x30 byte, letti per ogni oggetto anche se inattivo).
/// I primi 0x1e byte sono il blocco "object info" OAMD; poi +0x20 dialog indication (0-3),
/// +0x24 modalità di rendering cuffie (0-3), +0x28 flag 0/1, +0x2c tipo (0 = normale).
/// Valori di default presi da DolbyHrtfEnc.dll (FUN_…).
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct GranuleMeta {
    pub raw: [u8; 0x30],
}
const _: () = assert!(std::mem::size_of::<GranuleMeta>() == 0x30);

impl GranuleMeta {
    /// Oggetto statico (bed) in posizione (x, y, z), coordinate Dolby 0..1:
    /// x sinistra→destra, y fronte→retro, z pavimento→soffitto.
    pub fn bed(x: f32, y: f32, z: f32) -> Self {
        let mut raw = [0u8; 0x30];
        raw[0x02..0x04].copy_from_slice(&0x1000u16.to_le_bytes());
        raw[0x04..0x06].copy_from_slice(&0x7c00u16.to_le_bytes());
        for (off, v) in [(0x06, x), (0x08, y), (0x0a, z)] {
            raw[off..off + 2].copy_from_slice(&((v * 32767.0) as i16).to_le_bytes());
        }
        raw[0x0d] = 1;
        raw[0x0e] = 1;
        raw[0x1a..0x1c].copy_from_slice(&2u16.to_le_bytes());
        GranuleMeta { raw }
    }

    /// Modalità di rendering cuffie OAMD (+0x24, 0-3): con 0 il modulo non virtualizza,
    /// con 1 coincide con Windows.
    pub fn set_headphone_render_mode(&mut self, mode: u8) {
        assert!(mode <= 3);
        self.raw[0x24..0x28].copy_from_slice(&(mode as u32).to_le_bytes());
    }
}

/// Posizioni nominali dei 17 bed (ordine AudioObjectType). Per i bed il canale è identificato
/// dall'indice; le posizioni seguono la disposizione standard 7.1.4 (+ bottom e back center).
pub const BED_POSITIONS: [(f32, f32, f32); NUM_BEDS] = [
    (0.0, 0.0, 0.0), // FL
    (1.0, 0.0, 0.0), // FR
    (0.5, 0.0, 0.0), // FC
    (0.5, 0.0, 0.0), // LFE
    (0.0, 0.5, 0.0), // SL
    (1.0, 0.5, 0.0), // SR
    (0.0, 1.0, 0.0), // BL
    (1.0, 1.0, 0.0), // BR
    (0.0, 0.0, 1.0), // TFL
    (1.0, 0.0, 1.0), // TFR
    (0.0, 1.0, 1.0), // TBL
    (1.0, 1.0, 1.0), // TBR
    (0.0, 0.0, 0.0), // BFL
    (1.0, 0.0, 0.0), // BFR
    (0.0, 1.0, 0.0), // BBL
    (1.0, 1.0, 0.0), // BBR
    (0.5, 1.0, 0.0), // BC
];

/// Posizione degli oggetti non bed (bypass e dinamici) finché non ne arrivano di propri.
pub const DEFAULT_OBJECT_POSITION: (f32, f32, f32) = (0.5, 0.5, 0.0);

/// Voce dell'array di 147 oggetti passato a EncodeAudioData (0xa0 byte).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ObjectEntry {
    pub samples: *const f32,
    pub metadata: *const GranuleMeta,
    pub active: u8,
    /// +0x11..0x14 flag di presenza metadati dinamici, poi i metadati: tutti a zero per i bed.
    pub rest: [u8; 0x8f],
}
const _: () = assert!(std::mem::size_of::<ObjectEntry>() == 0xa0);

impl ObjectEntry {
    pub const EMPTY: ObjectEntry = ObjectEntry {
        samples: std::ptr::null(),
        metadata: std::ptr::null(),
        active: 0,
        rest: [0; 0x8f],
    };
}

/// Primo argomento di EncodeAudioData (nello shell: CDolbyHrtfEncoderEngine+[…]).
#[repr(C)]
pub struct EncodeInput {
    pub entries: *mut ObjectEntry,
    pub granule_offsets: *const i16,
    pub reserved: [u64; 2],
}
const _: () = assert!(std::mem::size_of::<EncodeInput>() == 0x20);
