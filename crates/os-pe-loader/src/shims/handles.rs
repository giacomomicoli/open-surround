//! Oggetti kernel: eventi, mutex, thread (CreateThread con TEB proprio), file mapping, attese.
//!
//! Come su Windows (doc sez. 12.12, punto 6):
//! - ogni Create/Open restituisce un handle **distinto**; gli handle sullo stesso oggetto ne
//!   condividono lo stato e l'oggetto vive finché vive l'ultimo (`Arc`);
//! - eventi, mutex e file mapping con nome stanno in un solo spazio dei nomi, che tiene
//!   riferimenti deboli: il nome sparisce con l'oggetto;
//! - ogni Create imposta l'ultimo errore: `ERROR_ALREADY_EXISTS` (183) se l'oggetto con quel
//!   nome esisteva già (e ne restituisce un handle nuovo), 0 altrimenti; un nome già usato da
//!   un oggetto di altro tipo dà NULL con `ERROR_INVALID_HANDLE`.
//!
//! Attese: ogni cambiamento di stato che può soddisfare un'attesa (SetEvent, PulseEvent,
//! ReleaseMutex, fine di un thread) avviene con il lock `SIGNAL` preso e sveglia tutti gli
//! attendenti sulla sua condvar; chi attende controlla e consuma con lo stesso lock, quindi
//! `WaitForMultipleObjects(bWaitAll)` è atomico e nessuna attesa fa polling.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::teb;

const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 0x102;
const WAIT_FAILED: u32 = 0xFFFF_FFFF;
const STILL_ACTIVE: u32 = 259;
const INFINITE: u32 = 0xFFFF_FFFF;
const MAXIMUM_WAIT_OBJECTS: u32 = 64;

pub(crate) const ERROR_FILE_NOT_FOUND: u32 = 2;
pub(crate) const ERROR_INVALID_HANDLE: u32 = 6;
pub(crate) const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
pub(crate) const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_ALREADY_EXISTS: u32 = 183;
const ERROR_NOT_OWNER: u32 = 288;

/// Valori magic per gli handle standard.
pub const STD_INPUT: u64 = 0x1000_0010;
pub const STD_OUTPUT: u64 = 0x1000_0011;
pub const STD_ERROR: u64 = 0x1000_0012;

/// Pseudo-handle di GetCurrentProcess e GetCurrentThread: CloseHandle li accetta senza effetto.
const CURRENT_PROCESS: u64 = u64::MAX;
const CURRENT_THREAD: u64 = u64::MAX - 1;

/// Base degli handle assegnati da noi (lontana da qualunque mapping del processo e dagli
/// HMODULE fittizi di `libloader`).
const HANDLE_BASE: u64 = 0x7000_0000_0000;

pub(crate) enum Kind {
    Event { manual: bool, st: Mutex<EventState> },
    Mutex(Mutex<MutexState>),
    Thread(Arc<ThreadState>),
    /// Memoria condivisa di CreateFileMappingW (vedi `mm`): il memfd si chiude con l'ultimo
    /// handle; le viste aperte restano valide perché hanno il proprio mmap.
    Mapping { fd: OwnedFd, size: usize },
}

#[derive(Default)]
pub(crate) struct EventState {
    signaled: bool,
    /// Thread in attesa adesso (per PulseEvent).
    waiters: u32,
    /// Generazione dei pulse: libera solo chi attendeva già quando il pulse è arrivato.
    pulse_gen: u64,
    /// Attendenti che quel pulse può ancora liberare (tutti con reset manuale, uno altrimenti).
    pulse_left: u32,
}

#[derive(Default)]
pub(crate) struct MutexState {
    owner: u32, // tid, 0 = libero
    count: u32, // ricorsione
}

pub struct ThreadState {
    exit_code: Mutex<Option<u32>>,
    tid: AtomicU32,
}

pub(crate) struct Obj {
    name: Option<String>,
    pub(crate) kind: Kind,
}

#[derive(Default)]
struct Table {
    handles: HashMap<u64, Arc<Obj>>,
    names: HashMap<String, Weak<Obj>>,
}

static TABLE: LazyLock<Mutex<Table>> = LazyLock::new(|| Mutex::new(Table::default()));
static NEXT_HANDLE: AtomicU32 = AtomicU32::new(1);

/// Lock dei cambiamenti di stato che soddisfano le attese (ordine: SIGNAL, poi lo stato
/// dell'oggetto; mai TABLE dentro SIGNAL).
static SIGNAL: Mutex<()> = Mutex::new(());
static SIGNAL_CV: Condvar = Condvar::new();

/// Thread vivi per tid (creati con CreateThread).
pub static THREADS: LazyLock<Mutex<HashMap<u32, Arc<ThreadState>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn insert_handle(table: &mut Table, obj: Arc<Obj>) -> u64 {
    let h = HANDLE_BASE + 8 * NEXT_HANDLE.fetch_add(1, Ordering::SeqCst) as u64;
    table.handles.insert(h, obj);
    h
}

fn register(kind: Kind) -> u64 {
    insert_handle(&mut TABLE.lock().unwrap(), Arc::new(Obj { name: None, kind }))
}

pub(crate) fn get(h: u64) -> Option<Arc<Obj>> {
    TABLE.lock().unwrap().handles.get(&h).cloned()
}

/// Create* con nome facoltativo. `make` costruisce l'oggetto solo se non esiste già
/// (errore Win32 se fallisce).
pub(crate) fn create_object(name: &str, same_kind: fn(&Kind) -> bool, make: impl FnOnce() -> Result<Kind, u32>) -> u64 {
    let mut t = TABLE.lock().unwrap();
    if !name.is_empty() {
        if let Some(existing) = t.names.get(name).and_then(Weak::upgrade) {
            if !same_kind(&existing.kind) {
                drop(t);
                teb::set_last_error(ERROR_INVALID_HANDLE);
                return 0;
            }
            let h = insert_handle(&mut t, existing);
            drop(t);
            teb::set_last_error(ERROR_ALREADY_EXISTS);
            return h;
        }
    }
    let kind = match make() {
        Ok(k) => k,
        Err(e) => {
            drop(t);
            teb::set_last_error(e);
            return 0;
        }
    };
    let obj = Arc::new(Obj { name: (!name.is_empty()).then(|| name.to_string()), kind });
    if !name.is_empty() {
        t.names.insert(name.to_string(), Arc::downgrade(&obj));
    }
    let h = insert_handle(&mut t, obj);
    drop(t);
    teb::set_last_error(0);
    h
}

/// Open* per nome: un handle nuovo sull'oggetto, `ERROR_FILE_NOT_FOUND` se il nome non c'è,
/// `ERROR_INVALID_HANDLE` se è di un altro tipo.
pub(crate) fn open_object(name: &str, same_kind: fn(&Kind) -> bool) -> u64 {
    let mut t = TABLE.lock().unwrap();
    let Some(obj) = t.names.get(name).and_then(Weak::upgrade) else {
        drop(t);
        teb::set_last_error(ERROR_FILE_NOT_FOUND);
        return 0;
    };
    if !same_kind(&obj.kind) {
        drop(t);
        teb::set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    insert_handle(&mut t, obj)
}

pub(crate) fn wide_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe {
        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
    }
}

fn is_event(k: &Kind) -> bool {
    matches!(k, Kind::Event { .. })
}

fn is_mutex(k: &Kind) -> bool {
    matches!(k, Kind::Mutex(_))
}

pub(crate) fn is_mapping(k: &Kind) -> bool {
    matches!(k, Kind::Mapping { .. })
}

// ---- Eventi ----

pub(crate) unsafe extern "win64" fn create_event_w(_sa: *const c_void, manual: i32, initial: i32, name: *const u16) -> u64 {
    create_object(&wide_string(name), is_event, || {
        Ok(Kind::Event { manual: manual != 0, st: Mutex::new(EventState { signaled: initial != 0, ..Default::default() }) })
    })
}

pub(crate) unsafe extern "win64" fn open_event_w(_access: u32, _inherit: i32, name: *const u16) -> u64 {
    open_object(&wide_string(name), is_event)
}

/// Applica `f` allo stato dell'evento `h` con il lock dei segnali preso, poi sveglia gli
/// attendenti.
fn with_event(h: u64, f: impl FnOnce(bool, &mut EventState)) -> i32 {
    let Some(obj) = get(h) else {
        teb::set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    let Kind::Event { manual, st } = &obj.kind else {
        teb::set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    {
        let _g = SIGNAL.lock().unwrap();
        f(*manual, &mut st.lock().unwrap());
    }
    SIGNAL_CV.notify_all();
    1
}

pub(crate) unsafe extern "win64" fn set_event(h: u64) -> i32 {
    with_event(h, |_, s| s.signaled = true)
}

pub(crate) unsafe extern "win64" fn reset_event(h: u64) -> i32 {
    with_event(h, |_, s| s.signaled = false)
}

/// PulseEvent: libera chi attende in questo momento (tutti con reset manuale, uno con reset
/// automatico) e lascia l'evento non segnalato; chi arriva dopo non vede il pulse.
pub(crate) unsafe extern "win64" fn pulse_event(h: u64) -> i32 {
    with_event(h, |manual, s| {
        s.signaled = false;
        if s.waiters > 0 {
            s.pulse_gen += 1;
            s.pulse_left = if manual { s.waiters } else { 1 };
        }
    })
}

// ---- Mutex ----

pub(crate) unsafe extern "win64" fn create_mutex_w(_sa: *const c_void, initial: i32, name: *const u16) -> u64 {
    // Su un mutex con nome già esistente `initial` non dà la proprietà (come Windows).
    create_object(&wide_string(name), is_mutex, || {
        let st = if initial != 0 { MutexState { owner: teb::gettid_win(), count: 1 } } else { MutexState::default() };
        Ok(Kind::Mutex(Mutex::new(st)))
    })
}

pub(crate) unsafe extern "win64" fn open_mutex_w(_access: u32, _inherit: i32, name: *const u16) -> u64 {
    open_object(&wide_string(name), is_mutex)
}

pub(crate) unsafe extern "win64" fn release_mutex(h: u64) -> i32 {
    let Some(obj) = get(h) else {
        teb::set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    let Kind::Mutex(st) = &obj.kind else {
        teb::set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    {
        let _g = SIGNAL.lock().unwrap();
        let mut s = st.lock().unwrap();
        if s.owner != teb::gettid_win() || s.count == 0 {
            drop(s);
            teb::set_last_error(ERROR_NOT_OWNER);
            return 0;
        }
        s.count -= 1;
        if s.count == 0 {
            s.owner = 0;
        }
    }
    SIGNAL_CV.notify_all();
    1
}

// ---- Thread ----

// Punto di ritorno per ExitThread (doc sez. 12.12, punto 7). `os_pe_loader_call_thread_entry`
// salva i registri callee-saved SysV, MXCSR e la control word x87, registra lo stack pointer
// in `*exit_sp` e chiama la funzione del thread con ABI win64. `os_pe_loader_exit_thread_jump`
// riporta lo stack a quel punto e prosegue come un ritorno con `code`: i frame scartati sono
// quelli del modulo (win64, nessun distruttore) e dello shim ExitThread (nessun valore con
// Drop vivo al salto), quindi non serve alcun unwinding, che `pthread_exit` farebbe passando
// per frame senza `.eh_frame`.
std::arch::global_asm!(
    ".text",
    ".globl os_pe_loader_call_thread_entry",
    ".type os_pe_loader_call_thread_entry,@function",
    "os_pe_loader_call_thread_entry:", // rdi = start (win64), rsi = param, rdx = *mut usize
    "push rbp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "sub rsp, 40", // 32 byte di shadow space win64 + 8: rsp allineato a 16 alla call
    "stmxcsr [rsp + 32]",
    "fnstcw [rsp + 36]",
    "mov [rdx], rsp",
    "mov rcx, rsi",
    "call rdi",
    "2:", // eax = codice d'uscita (ritorno naturale o ExitThread)
    "ldmxcsr [rsp + 32]",
    "fldcw [rsp + 36]",
    "add rsp, 40",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "ret",
    ".size os_pe_loader_call_thread_entry, . - os_pe_loader_call_thread_entry",
    ".globl os_pe_loader_exit_thread_jump",
    ".type os_pe_loader_exit_thread_jump,@function",
    "os_pe_loader_exit_thread_jump:", // rdi = stack pointer salvato, esi = codice
    "mov rsp, rdi",
    "mov eax, esi",
    "jmp 2b",
    ".size os_pe_loader_exit_thread_jump, . - os_pe_loader_exit_thread_jump",
);

extern "C" {
    fn os_pe_loader_call_thread_entry(start: usize, param: usize, exit_sp: *mut usize) -> u32;
    fn os_pe_loader_exit_thread_jump(sp: usize, code: u32) -> !;
}

thread_local! {
    /// Stack pointer del punto di ritorno di ExitThread (0 = nessuno: thread non nostro, o
    /// funzione del thread già terminata).
    static EXIT_SP: Cell<usize> = const { Cell::new(0) };
}

pub(crate) unsafe extern "win64" fn create_thread(
    _attrs: *const c_void,
    _stack: usize,
    start: *mut c_void,
    param: *mut c_void,
    _flags: u32,
    tid_out: *mut u32,
) -> u64 {
    if start.is_null() {
        teb::set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let state = Arc::new(ThreadState { exit_code: Mutex::new(None), tid: AtomicU32::new(0) });
    let state2 = state.clone();
    // Il tid lo conosce solo il thread nuovo: ce lo passa prima di eseguire il modulo.
    let (tx, rx) = std::sync::mpsc::channel::<u32>();
    let builder = std::thread::Builder::new().stack_size(4 * 1024 * 1024);
    let start_addr = start as usize;
    let param_addr = param as usize;
    let spawned = builder.spawn(move || {
        // TEB dedicato prima di qualsiasi codice del modulo: il GS base ereditato punta
        // al TEB del thread padre e andrebbe a leggere i suoi TLS.
        teb::install();
        let tid = teb::gettid_win();
        state2.tid.store(tid, Ordering::SeqCst);
        THREADS.lock().unwrap().insert(tid, state2.clone());
        let _ = tx.send(tid);
        crate::loader::notify_thread(crate::loader::DLL_THREAD_ATTACH);
        let code = EXIT_SP.with(|sp| {
            let code = unsafe { os_pe_loader_call_thread_entry(start_addr, param_addr, sp.as_ptr()) };
            sp.set(0);
            code
        });
        thread_teardown(&state2, code);
    });
    match spawned {
        Ok(_detached) => {
            if !tid_out.is_null() {
                if let Ok(tid) = rx.recv_timeout(Duration::from_millis(1000)) {
                    unsafe { tid_out.write(tid) };
                }
            }
            register(Kind::Thread(state))
        }
        Err(_) => {
            teb::set_last_error(ERROR_NOT_ENOUGH_MEMORY);
            0
        }
    }
}

/// Fine di un thread creato con CreateThread, nell'ordine di Windows (LdrShutdownThread):
/// callback FLS → DLL_THREAD_DETACH → GS azzerato e TEB liberato → handle segnalato.
fn thread_teardown(state: &Arc<ThreadState>, code: u32) {
    teb::run_thread_detach();
    crate::loader::notify_thread(crate::loader::DLL_THREAD_DETACH);
    teb::uninstall();
    THREADS.lock().unwrap().remove(&state.tid.load(Ordering::SeqCst));
    thread_finished(state, code);
}

/// Codice d'uscita registrato e handle del thread segnalato.
pub fn thread_finished(state: &Arc<ThreadState>, code: u32) {
    {
        let _g = SIGNAL.lock().unwrap();
        *state.exit_code.lock().unwrap() = Some(code);
    }
    SIGNAL_CV.notify_all();
}

pub(crate) unsafe extern "win64" fn exit_thread(code: u32) -> ! {
    let sp = EXIT_SP.with(Cell::get);
    if sp == 0 {
        // Thread non creato da CreateThread (principale, PipeWire…): non c'è un punto di
        // ritorno a cui tornare senza unwinding. Su Windows terminerebbe solo questo thread.
        eprintln!("[os-pe-loader] ExitThread({code}) su un thread non creato da CreateThread: non supportato");
        std::process::abort();
    }
    unsafe { os_pe_loader_exit_thread_jump(sp, code) }
}

pub(crate) unsafe extern "win64" fn get_exit_code_thread(h: u64, out: *mut u32) -> i32 {
    match get(h).as_deref().map(|o| &o.kind) {
        Some(Kind::Thread(state)) => {
            let code = state.exit_code.lock().unwrap().unwrap_or(STILL_ACTIVE);
            if !out.is_null() {
                unsafe { out.write(code) };
            }
            1
        }
        _ => {
            teb::set_last_error(ERROR_INVALID_HANDLE);
            0
        }
    }
}

pub fn thread_exit_code(state: &Arc<ThreadState>) -> Option<u32> {
    *state.exit_code.lock().unwrap()
}

pub(crate) unsafe extern "win64" fn free_library_and_exit_thread(_hmod: *mut c_void, code: u32) -> ! {
    // FreeLibrary non scarica nulla (il modulo resta per la vita del processo).
    unsafe { exit_thread(code) }
}

// ---- Attese ----

/// Pronto senza consumare; `start_gen` è la generazione dei pulse all'inizio dell'attesa.
fn ready(obj: &Obj, start_gen: u64, tid: u32) -> bool {
    match &obj.kind {
        Kind::Event { st, .. } => {
            let s = st.lock().unwrap();
            s.signaled || (s.pulse_gen > start_gen && s.pulse_left > 0)
        }
        Kind::Mutex(st) => {
            let s = st.lock().unwrap();
            s.owner == 0 || s.owner == tid
        }
        Kind::Thread(state) => state.exit_code.lock().unwrap().is_some(),
        Kind::Mapping { .. } => false,
    }
}

/// Consuma un oggetto pronto: reset automatico, proprietà del mutex, quota di un pulse.
fn consume(obj: &Obj, tid: u32) {
    match &obj.kind {
        Kind::Event { manual, st } => {
            let mut s = st.lock().unwrap();
            if s.signaled {
                if !*manual {
                    s.signaled = false;
                }
            } else {
                s.pulse_left -= 1;
            }
        }
        Kind::Mutex(st) => {
            let mut s = st.lock().unwrap();
            s.owner = tid;
            s.count += 1;
        }
        Kind::Thread(_) | Kind::Mapping { .. } => {}
    }
}

fn wait_objects(objs: &[Arc<Obj>], wait_all: bool, timeout_ms: u32) -> u32 {
    if objs.iter().any(|o| is_mapping(&o.kind)) {
        teb::set_last_error(ERROR_INVALID_HANDLE);
        return WAIT_FAILED;
    }
    let tid = teb::gettid_win();
    let deadline = (timeout_ms != INFINITE).then(|| Instant::now() + Duration::from_millis(timeout_ms as u64));
    let mut guard = SIGNAL.lock().unwrap();
    let start_gen: Vec<u64> = objs
        .iter()
        .map(|o| match &o.kind {
            Kind::Event { st, .. } => {
                let mut s = st.lock().unwrap();
                s.waiters += 1;
                s.pulse_gen
            }
            _ => 0,
        })
        .collect();
    let result = loop {
        let is_ready = |i: usize| ready(&objs[i], start_gen[i], tid);
        if wait_all {
            if (0..objs.len()).all(is_ready) {
                objs.iter().for_each(|o| consume(o, tid));
                break WAIT_OBJECT_0;
            }
        } else if let Some(i) = (0..objs.len()).find(|&i| is_ready(i)) {
            consume(&objs[i], tid);
            break WAIT_OBJECT_0 + i as u32;
        }
        match deadline {
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    break WAIT_TIMEOUT;
                }
                guard = SIGNAL_CV.wait_timeout(guard, d - now).unwrap().0;
            }
            None => guard = SIGNAL_CV.wait(guard).unwrap(),
        }
    };
    for o in objs {
        if let Kind::Event { st, .. } = &o.kind {
            st.lock().unwrap().waiters -= 1;
        }
    }
    drop(guard);
    result
}

pub(crate) unsafe extern "win64" fn wait_for_single_object(h: u64, timeout_ms: u32) -> u32 {
    match get(h) {
        Some(obj) => wait_objects(&[obj], false, timeout_ms),
        None => {
            teb::set_last_error(ERROR_INVALID_HANDLE);
            WAIT_FAILED
        }
    }
}

pub(crate) unsafe extern "win64" fn wait_for_single_object_ex(h: u64, timeout_ms: u32, _alertable: i32) -> u32 {
    unsafe { wait_for_single_object(h, timeout_ms) }
}

pub(crate) unsafe extern "win64" fn wait_for_multiple_objects(count: u32, handles: *const u64, wait_all: i32, timeout_ms: u32) -> u32 {
    if count == 0 || count > MAXIMUM_WAIT_OBJECTS || handles.is_null() {
        teb::set_last_error(ERROR_INVALID_PARAMETER);
        return WAIT_FAILED;
    }
    let hs = unsafe { std::slice::from_raw_parts(handles, count as usize) };
    let Some(objs) = hs.iter().map(|&h| get(h)).collect::<Option<Vec<_>>>() else {
        teb::set_last_error(ERROR_INVALID_HANDLE);
        return WAIT_FAILED;
    };
    wait_objects(&objs, wait_all != 0, timeout_ms)
}

pub(crate) unsafe extern "win64" fn close_handle(h: u64) -> i32 {
    if matches!(h, STD_INPUT | STD_OUTPUT | STD_ERROR | CURRENT_PROCESS | CURRENT_THREAD) {
        return 1;
    }
    let mut t = TABLE.lock().unwrap();
    let Some(obj) = t.handles.remove(&h) else {
        drop(t);
        teb::set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    // Ultimo riferimento: il nome si libera insieme all'oggetto.
    if let Some(name) = &obj.name {
        let ours = t.names.get(name).is_some_and(|w| std::ptr::eq(w.as_ptr(), Arc::as_ptr(&obj)));
        if ours && Arc::strong_count(&obj) == 1 {
            t.names.remove(name);
        }
    }
    drop(t);
    drop(obj); // fuori dal lock: per un mapping chiude il memfd
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    #[test]
    fn named_event_create_open_close() {
        teb::install();
        let name = wide("Local\\test-named-event");
        unsafe {
            let a = create_event_w(std::ptr::null(), 1, 0, name.as_ptr());
            assert_ne!(a, 0);
            assert_eq!(teb::last_error(), 0);
            let b = create_event_w(std::ptr::null(), 1, 0, name.as_ptr());
            assert_eq!(teb::last_error(), ERROR_ALREADY_EXISTS);
            assert_ne!(a, b, "handle distinti sullo stesso oggetto");
            let c = open_event_w(0, 0, name.as_ptr());
            assert_ne!(c, 0);
            // stato condiviso
            assert_eq!(set_event(b), 1);
            assert_eq!(wait_for_single_object(c, 0), WAIT_OBJECT_0);
            // il primo CloseHandle non chiude l'oggetto per gli altri
            assert_eq!(close_handle(a), 1);
            assert_eq!(close_handle(a), 0, "handle già chiuso");
            assert_eq!(wait_for_single_object(c, 0), WAIT_OBJECT_0);
            assert_eq!(close_handle(b), 1);
            assert_ne!(open_event_w(0, 0, name.as_ptr()), 0);
            assert_eq!(close_handle(c), 1);
            // gli handle aperti sopra con Open restano: chiudiamo e verifichiamo il nome
            let mut t = TABLE.lock().unwrap();
            let left: Vec<u64> = t.handles.iter().filter(|(_, o)| o.name.as_deref() == Some("Local\\test-named-event")).map(|(&h, _)| h).collect();
            drop(t);
            for h in left {
                close_handle(h);
            }
            t = TABLE.lock().unwrap();
            assert!(!t.names.contains_key("Local\\test-named-event"));
            drop(t);
            // nome libero: Open fallisce, Create ne fa uno nuovo
            assert_eq!(open_event_w(0, 0, name.as_ptr()), 0);
            assert_eq!(teb::last_error(), ERROR_FILE_NOT_FOUND);
            let d = create_event_w(std::ptr::null(), 0, 0, name.as_ptr());
            assert_eq!(teb::last_error(), 0);
            assert_eq!(wait_for_single_object(d, 0), WAIT_TIMEOUT, "oggetto nuovo, non segnalato");
            // stesso nome, altro tipo
            assert_eq!(create_mutex_w(std::ptr::null(), 0, name.as_ptr()), 0);
            assert_eq!(teb::last_error(), ERROR_INVALID_HANDLE);
            close_handle(d);
        }
    }

    #[test]
    fn pulse_wakes_current_waiter_only() {
        teb::install();
        let ev = unsafe { create_event_w(std::ptr::null(), 0, 0, std::ptr::null()) };
        let woke = Arc::new(AtomicBool::new(false));
        let w = woke.clone();
        let t = std::thread::spawn(move || {
            teb::install();
            let r = unsafe { wait_for_single_object(ev, 5000) };
            w.store(true, Ordering::SeqCst);
            r
        });
        // aspetta che il thread sia in coda sull'evento
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let obj = get(ev).unwrap();
            let Kind::Event { st, .. } = &obj.kind else { unreachable!() };
            if st.lock().unwrap().waiters == 1 {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        unsafe { pulse_event(ev) };
        assert_eq!(t.join().unwrap(), WAIT_OBJECT_0);
        assert!(woke.load(Ordering::SeqCst));
        // dopo il pulse l'evento non è segnalato e un nuovo attendente non lo vede
        assert_eq!(unsafe { wait_for_single_object(ev, 10) }, WAIT_TIMEOUT);
        // pulse senza attendenti: nessun effetto
        unsafe { pulse_event(ev) };
        assert_eq!(unsafe { wait_for_single_object(ev, 0) }, WAIT_TIMEOUT);
        unsafe { close_handle(ev) };
    }

    #[test]
    fn wait_all_is_atomic_and_wakes() {
        teb::install();
        let a = unsafe { create_event_w(std::ptr::null(), 0, 0, std::ptr::null()) };
        let b = unsafe { create_event_w(std::ptr::null(), 0, 1, std::ptr::null()) };
        let hs = [a, b];
        // solo b segnalato: WaitAll non consuma niente
        assert_eq!(unsafe { wait_for_multiple_objects(2, hs.as_ptr(), 1, 0) }, WAIT_TIMEOUT);
        assert_eq!(unsafe { wait_for_single_object(b, 0) }, WAIT_OBJECT_0, "b ancora segnalato");
        unsafe { set_event(b) };
        let t = std::thread::spawn(move || {
            teb::install();
            unsafe { wait_for_multiple_objects(2, [a, b].as_ptr(), 1, 5000) }
        });
        std::thread::sleep(Duration::from_millis(20));
        unsafe { set_event(a) };
        assert_eq!(t.join().unwrap(), WAIT_OBJECT_0);
        // entrambi consumati (reset automatico)
        assert_eq!(unsafe { wait_for_multiple_objects(2, hs.as_ptr(), 0, 0) }, WAIT_TIMEOUT);
        // WaitAny restituisce l'indice
        unsafe { set_event(b) };
        assert_eq!(unsafe { wait_for_multiple_objects(2, hs.as_ptr(), 0, 0) }, WAIT_OBJECT_0 + 1);
        unsafe {
            close_handle(a);
            close_handle(b);
        }
    }

    #[test]
    fn mutex_ownership() {
        teb::install();
        let m = unsafe { create_mutex_w(std::ptr::null(), 1, std::ptr::null()) };
        assert_eq!(unsafe { wait_for_single_object(m, 0) }, WAIT_OBJECT_0, "ricorsivo");
        let other = std::thread::spawn(move || {
            teb::install();
            unsafe { (wait_for_single_object(m, 0), release_mutex(m)) }
        });
        assert_eq!(other.join().unwrap(), (WAIT_TIMEOUT, 0));
        unsafe {
            assert_eq!(release_mutex(m), 1);
            assert_eq!(release_mutex(m), 1);
            assert_eq!(release_mutex(m), 0);
            assert_eq!(teb::last_error(), ERROR_NOT_OWNER);
            close_handle(m);
        }
    }

    // ---- fine dei thread (punto 7) ----

    static FLS_INDEX: AtomicU32 = AtomicU32::new(u32::MAX);
    static FLS_RUNS: AtomicU32 = AtomicU32::new(0);
    static FLS_SAW_VALUE: AtomicU32 = AtomicU32::new(0);

    /// Callback FLS che usa gli shim del contesto del thread, come il distruttore per-thread
    /// del CRT MSVC: prima doveva andare in panic per doppio prestito.
    unsafe extern "win64" fn fls_cb(value: *mut c_void) {
        unsafe {
            crate::teb::set_last_error(0x1234);
            let idx = FLS_INDEX.load(Ordering::SeqCst);
            let _ = crate::shims::fls_get_value(idx);
            crate::shims::fls_set_value(idx, std::ptr::null_mut());
        }
        FLS_SAW_VALUE.fetch_add(value as u32, Ordering::SeqCst);
        FLS_RUNS.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "win64" fn thread_returns(param: *mut c_void) -> u32 {
        unsafe { crate::shims::fls_set_value(FLS_INDEX.load(Ordering::SeqCst), param) };
        42
    }

    unsafe extern "win64" fn thread_exits(param: *mut c_void) -> u32 {
        unsafe {
            crate::shims::fls_set_value(FLS_INDEX.load(Ordering::SeqCst), param);
            deep_exit(3)
        }
    }

    /// Qualche frame win64 in mezzo prima di ExitThread.
    unsafe extern "win64" fn deep_exit(n: u32) -> u32 {
        if n == 0 {
            unsafe { exit_thread(43) }
        }
        unsafe { deep_exit(n - 1) + 1 }
    }

    #[test]
    fn thread_end_natural_and_exit_thread() {
        teb::install();
        let idx = unsafe { crate::shims::fls_alloc(Some(fls_cb)) };
        FLS_INDEX.store(idx, Ordering::SeqCst);
        for (start, param, code) in [
            (thread_returns as unsafe extern "win64" fn(*mut c_void) -> u32, 1usize, 42u32),
            (thread_exits, 2, 43),
        ] {
            let before = FLS_RUNS.load(Ordering::SeqCst);
            let mut tid = 0u32;
            let h = unsafe { create_thread(std::ptr::null(), 0, start as *mut c_void, param as *mut c_void, 0, &mut tid) };
            assert_ne!(h, 0);
            assert_ne!(tid, 0);
            assert_eq!(unsafe { wait_for_single_object(h, 5000) }, WAIT_OBJECT_0, "handle del thread segnalato");
            let mut got = 0u32;
            assert_eq!(unsafe { get_exit_code_thread(h, &mut got) }, 1);
            assert_eq!(got, code);
            assert_eq!(FLS_RUNS.load(Ordering::SeqCst), before + 1, "callback FLS eseguita");
            assert!(!THREADS.lock().unwrap().contains_key(&tid));
            unsafe { close_handle(h) };
        }
        assert_eq!(FLS_SAW_VALUE.load(Ordering::SeqCst), 3, "valori FLS dei due thread");
        // il processo è vivo e il thread principale ha ancora il suo TEB
        assert!(teb::is_installed());
    }
}
