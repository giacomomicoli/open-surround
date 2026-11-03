//! os-dap-core: tutto ciò che open-surround passa a `DolbyAudioProcessing.dll` e che non dipende
//! dalla piattaforma (fase 2 della roadmap, doc sez. 12.5): blob Dolby e proprietà dell'endpoint,
//! configurazione dell'encoder, layout in memoria di oggetti e metadati, catena di guadagno e
//! instradamento bed/bypass, elaborazione offline WAV → WAV.
//!
//! Il modulo vero e proprio lo carica un [`Backend`]: su Windows il sistema (crate `os-dap-host`),
//! su Linux il loader PE nativo (fase 3). Entrambi ricevono dal nucleo esattamente gli stessi dati.

pub mod abi;
pub mod engine;
pub mod layout;
pub mod offline;
pub mod props;

pub use engine::{Backend, Engine, ModuleSetup};
pub use layout::Mode;
pub use props::{Guid, PropKey, Value};
