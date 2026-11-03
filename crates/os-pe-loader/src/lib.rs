//! os-pe-loader: loader PE32+ nativo per Linux (fase 3 della roadmap, doc sez. 12.5).
//!
//! Carica un DLL Windows x86-64 in un processo Linux: mappa le sezioni, applica le
//! rilocazioni, riempie la IAT con implementazioni Rust delle API Windows usate dal modulo
//! (ABI win64), installa un TEB sintetico nel segmento GS per il thread-local storage del
//! modulo, esegue DllMain ed espone gli export. Sopra ci vive il backend `os-dap-linux`,
//! che pilota il modulo con la stessa sequenza dell'host Windows.
//!
//! Le eccezioni SEH/C++ x64 (RtlUnwindEx e la famiglia RtlVirtualUnwind) non sono
//! implementate: nessun percorso verificato del modulo le attraversa (sui 78 casi di
//! regressione di Windows nemmeno), quindi i relativi shim terminano con un messaggio.

// La tabella degli shim e le vtable elencano funzioni come indirizzi: è il meccanismo
// stesso di una IAT.
#![allow(function_casts_as_integer)]

pub mod loader;
pub mod pe;
pub mod shims;
pub mod teb;

pub use loader::{image_containing, Image};

/// Attiva la stampa del log interno del modulo (equivalente di -v nell'host Windows:
/// ETW abilitato al livello massimo + OutputDebugString stampati + tracing COM).
pub fn set_verbose(on: bool) {
    shims::etw::VERBOSE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Prepara il thread corrente a eseguire codice del modulo (TEB + GS). Idempotente:
/// il loader la chiama, serve chiamarla a mano solo per thread gestiti fuori da
/// CreateThread.
pub fn install_thread() {
    teb::install();
}
