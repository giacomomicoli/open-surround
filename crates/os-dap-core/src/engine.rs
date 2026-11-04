//! Motore comune: prepara oggetti, metadati e buffer per EncodeAudioData e li passa al backend
//! di piattaforma, che possiede l'istanza del modulo.

use std::path::Path;

use anyhow::Result;

use crate::abi::{EncodeInput, GranuleMeta, ObjectEntry, BED_POSITIONS, DEFAULT_OBJECT_POSITION};
use crate::layout::{check_block_frames, encoder_config, Mode, NUM_ENTRIES, NUM_STATIC, OUT_CHANNELS};
use crate::props::{device_id, endpoint_properties, Guid, PropKey, Value, ENDPOINT_GUID};

/// Tutto ciò che serve a un backend per caricare e configurare il modulo, identico su ogni
/// piattaforma. La sequenza da replicare è quella di DolbyHrtfEnc.dll (doc sez. 12.6-12.7):
/// DllGetActivationFactory("Dolby.DolbyAudioProcessingModule") → IDAPModule2 →
/// Initialize(null, null) → SetAPOInitParameters (endpoint con `endpoint_props`, IMMDevice `device_id`
/// il cui IAudioSystemEffectsPropertyStore restituisce `fx_props`) → ConfigureEncoder(`encoder_config`).
pub struct ModuleSetup<'a> {
    pub dll: &'a Path,
    pub mode: Mode,
    pub encoder_config: [u32; 8],
    pub endpoint_guid: Guid,
    pub device_id: String,
    pub endpoint_props: Vec<(PropKey, Value)>,
    pub fx_props: Vec<(PropKey, Value)>,
    /// Stampa le chiamate del modulo all'host e il suo log interno.
    pub verbose: bool,
}

impl<'a> ModuleSetup<'a> {
    pub fn new(dll: &'a Path, mode: Mode, block_frames: usize, fx_props: Vec<(PropKey, Value)>, verbose: bool) -> Result<Self> {
        check_block_frames(block_frames)?;
        Ok(ModuleSetup {
            dll,
            mode,
            encoder_config: encoder_config(mode, block_frames),
            endpoint_guid: ENDPOINT_GUID,
            device_id: device_id(ENDPOINT_GUID),
            endpoint_props: endpoint_properties(ENDPOINT_GUID),
            fx_props,
            verbose,
        })
    }

    pub fn block_frames(&self) -> usize {
        self.encoder_config[1] as usize
    }
}

/// Un'istanza di `DolbyAudioProcessing.dll` caricata e configurata da una piattaforma.
pub trait Backend: Sized {
    /// Carica il modulo ed esegue la sequenza descritta in [`ModuleSetup`] fino a ConfigureEncoder.
    fn open(setup: &ModuleSetup) -> Result<Self>;

    /// IDAPModule2::EncodeAudioData (slot 5).
    ///
    /// # Safety
    /// `input` e i puntatori che contiene devono essere validi per un blocco di
    /// `block_frames` campioni; `out` deve avere spazio per `block_frames` × 8 float.
    unsafe fn encode(&mut self, input: *mut EncodeInput, out: *mut f32) -> Result<()>;

    /// IDAPModule2::ConfigureEncoder di nuovo, con la stessa configurazione di `open`: riporta
    /// il modulo allo stato appena configurato (filtri, leveler e smoothing azzerati), come
    /// un'istanza nuova (fase 4, doc sez. 12.13). Serve solo al tempo reale.
    fn reconfigure(&mut self) -> Result<()> {
        anyhow::bail!("riconfigurazione del modulo non implementata da questo backend")
    }
}

pub struct Engine<B: Backend> {
    backend: B,
    block_frames: usize,
    entries: Vec<ObjectEntry>,
    metadata: Vec<GranuleMeta>,
    granule_offsets: Vec<i16>,
    inputs: Vec<Vec<f32>>,
    silence: Vec<f32>,
    out: Vec<f32>,
}

impl<B: Backend> Engine<B> {
    pub fn open(setup: &ModuleSetup) -> Result<Self> {
        let block_frames = setup.block_frames();
        check_block_frames(block_frames)?;
        let backend = B::open(setup)?;
        let granules = 1;
        let metadata: Vec<GranuleMeta> = (0..NUM_ENTRIES)
            .map(|i| {
                let (x, y, z) = BED_POSITIONS.get(i).copied().unwrap_or(DEFAULT_OBJECT_POSITION);
                GranuleMeta::bed(x, y, z)
            })
            .flat_map(|m| std::iter::repeat_n(m, granules))
            .collect();
        Ok(Engine {
            backend,
            block_frames,
            entries: vec![ObjectEntry::EMPTY; NUM_ENTRIES],
            metadata,
            granule_offsets: vec![0; granules],
            inputs: vec![vec![0.0; block_frames]; NUM_STATIC],
            silence: vec![0.0; block_frames],
            // Il modulo può scrivere fino a 8 canali.
            out: vec![0.0; block_frames * 8],
        })
    }

    /// Modalità di rendering cuffie OAMD (+0x24 dei metadati, 0-3) per tutti gli oggetti.
    pub fn set_headphone_render_mode(&mut self, mode: u8) {
        self.metadata.iter_mut().for_each(|m| m.set_headphone_render_mode(mode));
    }

    pub fn block_frames(&self) -> usize {
        self.block_frames
    }

    /// Riporta il modulo allo stato appena configurato: vedi [`Backend::reconfigure`].
    pub fn reconfigure(&mut self) -> Result<()> {
        self.backend.reconfigure()
    }

    /// Buffer di un ingresso statico (bed 0-16 o oggetto bypass 17-18) da riempire prima di `process`.
    pub fn input_mut(&mut self, index: usize) -> &mut [f32] {
        &mut self.inputs[index]
    }

    /// Elabora un blocco: `active[i]` dice quali ingressi statici contengono segnale.
    /// Ritorna `block_frames` frame stereo interleaved.
    pub fn process(&mut self, active: &[bool; NUM_STATIC]) -> Result<&[f32]> {
        for (i, e) in self.entries.iter_mut().enumerate() {
            let on = i < NUM_STATIC && active[i];
            e.samples = if on { self.inputs[i].as_ptr() } else { self.silence.as_ptr() };
            e.metadata = &self.metadata[i];
            e.active = on as u8;
        }
        let mut input = EncodeInput {
            entries: self.entries.as_mut_ptr(),
            granule_offsets: self.granule_offsets.as_ptr(),
            reserved: [0; 2],
        };
        // SAFETY: entries, metadati, buffer e uscita sono dimensionati per block_frames e vivono
        // in self per tutta la chiamata.
        unsafe { self.backend.encode(&mut input, self.out.as_mut_ptr())? };
        Ok(&self.out[..self.block_frames * OUT_CHANNELS])
    }
}
