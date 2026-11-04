//! os-dap-core: tutto ciò che open-surround passa a `DolbyAudioProcessing.dll` e che non dipende
//! dalla piattaforma (fase 2 della roadmap, doc sez. 12.5): blob Dolby e proprietà dell'endpoint,
//! configurazione dell'encoder, layout in memoria di oggetti e metadati, catena di guadagno e
//! instradamento bed/bypass, elaborazione offline WAV → WAV e adattatore per il tempo reale
//! (fase 4: cicli di lunghezza qualsiasi → blocchi fissi del modulo).
//!
//! Il modulo vero e proprio lo carica un [`Backend`]: su Windows il sistema (crate `os-dap-host`),
//! su Linux il loader PE nativo (fase 3). Entrambi ricevono dal nucleo esattamente gli stessi dati.

pub mod abi;
pub mod engine;
pub mod layout;
pub mod offline;
pub mod options;
pub mod props;
pub mod realtime;

pub use engine::{Backend, Engine, ModuleSetup};
pub use layout::Mode;
pub use props::{Guid, PropKey, Value};
pub use realtime::{Source, Streamer};
