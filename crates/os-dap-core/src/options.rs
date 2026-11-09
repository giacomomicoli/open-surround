//! Opzioni del modulo comuni all'elaborazione offline e al nodo PipeWire (fase 4): da dove
//! viene il DLL, quali blob, modalità, blocco, rendering cuffie, traccia.

use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::engine::{Backend, Engine, ModuleSetup};
use crate::layout::Mode;
use crate::props::BlobSource;

#[cfg(windows)]
const DEFAULT_DLL: &str = r"C:\tools\dolby-bins\DolbyAudioProcessing.dll";
#[cfg(not(windows))]
const DEFAULT_DLL: &str = "DolbyAudioProcessing.dll";

pub struct ModuleOptions {
    pub dll: PathBuf,
    pub blobs: BlobSource,
    pub mode: Mode,
    pub block: usize,
    pub hp_render: u8,
    pub verbose: bool,
}

impl Default for ModuleOptions {
    fn default() -> Self {
        ModuleOptions {
            dll: PathBuf::from(DEFAULT_DLL),
            blobs: BlobSource { blob_skip: 8, ..Default::default() },
            mode: Mode::Headphones,
            block: 256,
            hp_render: 1,
            verbose: false,
        }
    }
}

impl ModuleOptions {
    /// Interpreta `flag` se è un'opzione del modulo (`val` legge il valore che segue) e
    /// restituisce `false` se non lo è: --dll, --capture, --profile, --endpoint, --mode,
    /// --init, --runtime, --blob-skip, --block, --hp-render, -v.
    pub fn parse_flag(&mut self, flag: &str, val: &mut dyn FnMut() -> Result<String>) -> Result<bool> {
        match flag {
            "--dll" => self.dll = val()?.into(),
            "--capture" => self.blobs.captures.push(PathBuf::from(val()?)),
            "--profile" => self.blobs.profile = Some(val()?),
            "--endpoint" => self.blobs.endpoint = Some(val()?.to_lowercase()),
            "--mode" => {
                self.mode = match val()?.as_str() {
                    "cuffie" => Mode::Headphones,
                    "speaker" => Mode::Speakers,
                    m => bail!("modalità sconosciuta: {m} (cuffie|speaker)"),
                }
            }
            "--init" => self.blobs.init = Some(PathBuf::from(val()?)),
            "--runtime" => self.blobs.runtime = Some(PathBuf::from(val()?)),
            "--blob-skip" => self.blobs.blob_skip = val()?.parse()?,
            "--block" => self.block = val()?.parse()?,
            "--hp-render" => self.hp_render = val()?.parse()?,
            "-v" | "--verbose" => self.verbose = true,
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Legge i blob e apre il modulo con il backend `B`, configurato fino a ConfigureEncoder.
    pub fn open<B: Backend>(&self) -> Result<Engine<B>> {
        let fx_props = self.blobs.fx_values(self.mode)?;
        let setup = ModuleSetup::new(&self.dll, self.mode, self.block, fx_props, self.verbose)?;
        let mut engine = Engine::<B>::open(&setup)?;
        engine.set_headphone_render_mode(self.hp_render.min(3));
        Ok(engine)
    }
}
