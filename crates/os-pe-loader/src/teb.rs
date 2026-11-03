//! TEB sintetico e GS base: il modulo Windows (compilato x86-64) accede al suo storage
//! thread-local con `mov rax, gs:[0x58]`, come fa su Windows dove il TEB è il segmento GS.
//! Su Linux x86-64 il registro GS è libero (glibc usa FS), quindi ogni thread che esegue
//! codice del modulo si installa un TEB nostro con l'array dei puntatori TLS a +0x58.
//!
//! Nota: il GS base viene ereditato dai thread creati da un thread che lo ha impostato
//! (verificato sperimentalmente): per questo ogni trampoline di `CreateThread` installa
//! subito il proprio TEB, prima di eseguire il codice del modulo.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex};

const ARCH_SET_GS: libc::c_ulong = 0x1001;

/// Numero massimo di indici TLS di modulo (ogni DLL caricata ne usa uno).
const MAX_TLS_MODULES: usize = 64;

/// Offsets del TEB che il modulo o il suo CRT possono leggere.
mod offsets {
    pub const STACK_BASE: usize = 0x08;
    pub const STACK_LIMIT: usize = 0x10;
    pub const SELF: usize = 0x30;
    pub const CLIENT_ID: usize = 0x40;
    pub const TLS_ARRAY: usize = 0x58;
    pub const PEB: usize = 0x60;
    pub const LAST_ERROR: usize = 0x68;
}

/// PEB sintetico condiviso da tutti i thread: il CRT del modulo legge NtGlobalFlag (+0xBC) e
/// i ProcessParameters (+0x20, con `Flags` a +8) attraverso gs:[0x30] → TEB+0x60.
struct Peb {
    _bytes: Box<[u8; 0x100]>,
    /// I parametri sono referenziati da PEB+0x20 (puntatore copiato in `_bytes`): il campo
    /// li tiene in vita (per questo "non è mai letto" direttamente).
    #[allow(dead_code)]
    params: Box<[u8; 0x80]>,
    _strings: Vec<Box<[u16]>>, // tiene in vita i buffer di UNICODE_STRING
}

fn put_unicode_string(params: &mut [u8], at: usize, s: &[u16], keep: &mut Vec<Box<[u16]>>) {
    // UNICODE_STRING { u16 Length (senza NUL); u16 MaximumLength (con NUL); [pad 4]; ptr }
    params[at..at + 2].copy_from_slice(&((s.len() as u16 * 2).to_le_bytes()));
    params[at + 2..at + 4].copy_from_slice(&(((s.len() as u16 + 1) * 2).to_le_bytes()));
    params[at + 4..at + 8].copy_from_slice(&[0; 4]);
    let boxed: Box<[u16]> = s.to_vec().into();
    params[at + 8..at + 16].copy_from_slice(&(boxed.as_ptr() as u64).to_le_bytes());
    keep.push(boxed);
}

fn put_handle(params: &mut [u8], at: usize, h: u64) {
    params[at..at + 8].copy_from_slice(&h.to_le_bytes());
}

static PEB: std::sync::LazyLock<Box<Peb>> = std::sync::LazyLock::new(|| {
    use crate::shims::handles::{STD_ERROR, STD_INPUT, STD_OUTPUT};
    let mut peb = Box::new([0u8; 0x100]);
    let mut params = Box::new([0u8; 0x80]);
    let mut strings: Vec<Box<[u16]>> = Vec::new();
    // RTL_USER_PROCESS_PARAMETERS (x64)
    let total = params.len() as u32;
    params[0..4].copy_from_slice(&total.to_le_bytes()); // MaximumLength
    params[4..8].copy_from_slice(&(total - 4).to_le_bytes()); // Length
    // +0x8 Flags = 0 (è ciò che legge il CRT: bit 31 → 0)
    put_handle(&mut params[..], 0x20, STD_INPUT);
    put_handle(&mut params[..], 0x28, STD_OUTPUT);
    put_handle(&mut params[..], 0x30, STD_ERROR);
    let cwd: Vec<u16> = "C:\\".encode_utf16().chain(Some(0)).collect();
    put_unicode_string(&mut params[..], 0x38, &cwd, &mut strings); // CurrentDirectory.DosPath
    put_handle(&mut params[..], 0x48, 0); // CurrentDirectory.Handle
    put_unicode_string(&mut params[..], 0x50, &[], &mut strings); // DllPath vuota
    let image: Vec<u16> = "C:\\os-dap-linux.exe".encode_utf16().chain(Some(0)).collect();
    put_unicode_string(&mut params[..], 0x60, &image, &mut strings); // ImagePathName
    let cmdline: Vec<u16> = "os-dap-linux".encode_utf16().chain(Some(0)).collect();
    put_unicode_string(&mut params[..], 0x70, &cmdline, &mut strings); // CommandLine
    // PEB: +0x20 ProcessParameters, +0x30 ProcessHeap (= 1, come GetProcessHeap),
    // +0xBC NtGlobalFlag = 0 (già zero). BeingDebugged (+0x2) = 0.
    #[allow(clippy::let_and_return)]
    let _ = &params; // viva finché vive il PEB
    peb[0x20..0x28].copy_from_slice(&(params.as_ptr() as u64).to_le_bytes());
    peb[0x30..0x38].copy_from_slice(&1u64.to_le_bytes());
    Box::new(Peb { _bytes: peb, params, _strings: strings })
});

/// Limiti reali dello stack del thread corrente (il CRT del modulo li usa per validare
/// puntatori e per il probing).
fn real_stack_bounds() -> (usize, usize) {
    unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) == 0 {
            let mut base: *mut libc::c_void = std::ptr::null_mut();
            let mut size: libc::size_t = 0;
            if libc::pthread_attr_getstack(&attr, &mut base, &mut size) == 0 {
                let base = base as usize;
                return (base, base + size); // (limite basso, base alta)
            }
        }
        // ripiego: 8 MB sotto il puntatore attuale
        let sp = &0u8 as *const u8 as usize;
        (sp - 8 * 1024 * 1024, sp)
    }
}

pub struct TlsTemplate {
    /// Indice nella matrice TLS del TEB (assegnato dal loader a `AddressOfIndex` del modulo).
    pub index: u32,
    /// Byte iniziali del blocco per thread (copia di `[start, end)` della sezione TLS).
    pub raw: Vec<u8>,
    /// Zero fill dopo i byte iniziali.
    pub zero_fill: u32,
}

static TLS_MODULES: LazyLock<Mutex<Vec<TlsTemplate>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static TLS_NEXT_INDEX: AtomicU32 = AtomicU32::new(0);

/// Slot TlsAlloc/TlsSetValue (diversi dagli indici TLS "loader" sopra).
static SYS_TLS_SLOTS: LazyLock<Mutex<Vec<bool>>> = LazyLock::new(|| Mutex::new(Vec::new()));

pub fn alloc_tls_index() -> u32 {
    TLS_NEXT_INDEX.fetch_add(1, Ordering::SeqCst)
}

pub fn register_tls_module(template: TlsTemplate) {
    TLS_MODULES.lock().unwrap().push(template);
    // I thread già vivi con TEB (quello che sta caricando) allocano il blocco alla prossima
    // occasione; il loader chiama `ensure_blocks_for_current_thread` subito dopo.
}

pub fn sys_tls_alloc() -> Option<u32> {
    let mut slots = SYS_TLS_SLOTS.lock().unwrap();
    let idx = slots.iter().position(|used| !*used).unwrap_or_else(|| {
        slots.push(true);
        slots.len() - 1
    });
    slots[idx] = true;
    Some(idx as u32)
}

pub fn sys_tls_free(idx: u32) -> bool {
    let mut slots = SYS_TLS_SLOTS.lock().unwrap();
    if let Some(used) = slots.get_mut(idx as usize) {
        *used = false;
        true
    } else {
        false
    }
}

struct ThreadCtx {
    #[allow(dead_code)]
    teb: Box<[u64; 512]>,
    /// Puntatori ai blocchi TLS dei moduli, puntato dal TEB a +0x58.
    #[allow(dead_code)]
    tls_array: Box<[u64; MAX_TLS_MODULES]>,
    /// Blocchi TLS per indice modulo (allineati a 16).
    tls_blocks: RefCell<HashMap<u32, Box<[u8]>>>,
    /// Valori degli slot TlsAlloc di questo thread.
    sys_tls: RefCell<Vec<Option<usize>>>,
    /// Valori degli slot FlsAlloc di questo thread (con callback in `fls::CALLBACKS`).
    fls: RefCell<Vec<Option<usize>>>,
    last_error: Cell<u32>,
    com_initialized: Cell<bool>,
}

thread_local! {
    static CTX: RefCell<Option<ThreadCtx>> = const { RefCell::new(None) };
}

fn alloc_tls_block(t: &TlsTemplate) -> Box<[u8]> {
    let raw_len = t.raw.len();
    let total = raw_len + t.zero_fill as usize;
    let mut block = vec![0u8; total.max(16).next_multiple_of(16)].into_boxed_slice();
    block[..raw_len].copy_from_slice(&t.raw);
    block
}

/// Installa (se manca) il TEB di questo thread, con i blocchi TLS dei moduli già caricati.
/// Da chiamare su ogni thread che esegue codice del modulo (prima della prima chiamata).
pub fn install() {
    CTX.with_borrow_mut(|ctx| {
        if ctx.is_some() {
            return;
        }
        let mut teb: Box<[u64; 512]> = Box::new([0; 512]);
        let mut tls_array: Box<[u64; MAX_TLS_MODULES]> = Box::new([0; MAX_TLS_MODULES]);
        let templates = TLS_MODULES.lock().unwrap();
        let mut blocks = HashMap::new();
        for t in templates.iter() {
            let block = alloc_tls_block(t);
            tls_array[t.index as usize] = block.as_ptr() as u64;
            blocks.insert(t.index, block);
        }
        unsafe {
            let p = teb.as_mut_ptr();
            *p.add(offsets::SELF / 8) = p as u64;
            *p.add(offsets::TLS_ARRAY / 8) = tls_array.as_ptr() as u64;
            let (stack_low, stack_high) = real_stack_bounds();
            *p.add(offsets::STACK_LIMIT / 8) = stack_low as u64;
            *p.add(offsets::STACK_BASE / 8) = stack_high as u64;
            *p.add(offsets::PEB / 8) = PEB._bytes.as_ptr() as u64;
            *p.add(offsets::CLIENT_ID / 8) = std::process::id() as u64;
            *p.add(offsets::CLIENT_ID / 8 + 1) = libc::gettid() as u64;
            let rc = libc::syscall(libc::SYS_arch_prctl, ARCH_SET_GS, p as usize);
            assert_eq!(rc, 0, "arch_prctl(ARCH_SET_GS) fallito");
        }
        *ctx = Some(ThreadCtx {
            teb,
            tls_array,
            tls_blocks: RefCell::new(blocks),
            sys_tls: RefCell::new(Vec::new()),
            fls: RefCell::new(Vec::new()),
            last_error: Cell::new(0),
            com_initialized: Cell::new(false),
        });
    });
}

/// Alloca per questo thread i blocchi TLS dei moduli registrati dopo l'installazione
/// (chiamato dal loader appena il modulo dichiara il suo TLS).
pub fn ensure_blocks_for_current_thread() {
    CTX.with_borrow_mut(|ctx| {
        let Some(c) = ctx.as_mut() else { return };
        let templates = TLS_MODULES.lock().unwrap();
        for t in templates.iter() {
            c.tls_blocks.borrow_mut().entry(t.index).or_insert_with(|| alloc_tls_block(t));
            let block = c.tls_blocks.borrow()[&t.index].as_ptr();
            unsafe {
                let arr = c.tls_array.as_mut_ptr();
                *arr.add(t.index as usize) = block as u64;
            }
        }
    });
}

/// True se questo thread ha un TEB installato.
pub fn is_installed() -> bool {
    CTX.with_borrow(|ctx| ctx.is_some())
}

pub fn gettid_win() -> u32 {
    // Il modulo vede il thread id del TEB/ClientId; usiamo il tid di Linux, come fa Windows
    // con il tid del processo/cliente (valori diversi ma distinti per thread).
    unsafe { libc::gettid() as u32 }
}

/// Cleanup di fine thread: esegue le callback FLS e rilascia i blocchi TLS.
/// `ExitThread` e la trampoline di `CreateThread` la chiamano prima di terminare.
pub fn run_thread_detach() {
    CTX.with_borrow_mut(|ctx| {
        let Some(c) = ctx.as_mut() else { return };
        // Callback FLS (il CRT vi affida i distruttori per-thread).
        let fls_values: Vec<Option<usize>> = {
            let mut fls = c.fls.borrow_mut();
            let vals = fls.clone();
            fls.clear();
            vals
        };
        for (idx, value) in fls_values.iter().enumerate() {
            if let Some(v) = value {
                if let Some(cb) = crate::shims::fls_callback(idx as u32) {
                    let f: unsafe extern "win64" fn(*mut c_void) = unsafe { std::mem::transmute(cb) };
                    unsafe { f(*v as usize as *mut c_void) };
                }
            }
        }
    });
}

pub fn sys_tls_get(idx: u32) -> Option<usize> {
    CTX.with_borrow(|ctx| ctx.as_ref().and_then(|c| c.sys_tls.borrow().get(idx as usize).copied().flatten()))
}

pub fn sys_tls_set(idx: u32, value: Option<usize>) {
    CTX.with_borrow_mut(|ctx| {
        if let Some(c) = ctx.as_mut() {
            let mut slots = c.sys_tls.borrow_mut();
            if slots.len() <= idx as usize {
                slots.resize(idx as usize + 1, None);
            }
            slots[idx as usize] = value;
        }
    });
}

pub fn fls_get(idx: u32) -> Option<usize> {
    CTX.with_borrow(|ctx| ctx.as_ref().and_then(|c| c.fls.borrow().get(idx as usize).copied().flatten()))
}

pub fn fls_set(idx: u32, value: Option<usize>) {
    CTX.with_borrow_mut(|ctx| {
        if let Some(c) = ctx.as_mut() {
            let mut slots = c.fls.borrow_mut();
            if slots.len() <= idx as usize {
                slots.resize(idx as usize + 1, None);
            }
            slots[idx as usize] = value;
        }
    });
}

pub fn last_error() -> u32 {
    CTX.with_borrow(|ctx| ctx.as_ref().map(|c| c.last_error.get()).unwrap_or(0))
}

pub fn set_last_error(e: u32) {
    CTX.with_borrow_mut(|ctx| {
        if let Some(c) = ctx.as_mut() {
            c.last_error.set(e);
            unsafe {
                // Specchio nel TEB, per codice che legge gs:[0x68] direttamente.
                let p = c.teb.as_mut_ptr();
                *p.add(offsets::LAST_ERROR / 8) = e as u64;
            }
        }
    });
}

pub fn com_mark_initialized() -> bool {
    // Ritorna true se era già inizializzato (S_FALSE), false altrimenti (S_OK).
    CTX.with_borrow_mut(|ctx| ctx.as_mut().map(|c| c.com_initialized.replace(true)).unwrap_or(true))
}

#[cfg(test)]
mod tests {
    #[test]
    fn install_idempotent() {
        super::install();
        super::install();
        assert!(super::is_installed());
        // In un thread nuovo non c'è nulla finché non si installa.
        let installed = std::thread::spawn(|| super::is_installed()).join().unwrap();
        assert!(!installed);
    }
}
