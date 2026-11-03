//! Oggetti kernel: eventi, mutex, thread (CreateThread con TEB proprio), attese.
//! Gli handle sono valori opachi in una tabella globale; gli oggetti con nome finiscono in un
//! registro per nome, così OpenEventW/OpenMutexW trovano quelli creati prima.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::teb;

const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 0x102;
const WAIT_FAILED: u32 = 0xFFFF_FFFF;
const STILL_ACTIVE: u32 = 259;
const INFINITE: u32 = 0xFFFF_FFFF;
const ERROR_NOT_FOUND: u32 = 1168;

/// Valori magic per gli handle standard.
pub const STD_INPUT: u64 = 0x1000_0010;
pub const STD_OUTPUT: u64 = 0x1000_0011;
pub const STD_ERROR: u64 = 0x1000_0012;

/// Base degli handle assegnati da noi (lontana da qualunque mapping del processo).
const HANDLE_BASE: u64 = 0x7000_0000_0000;

enum Kind {
    Event { manual: bool, signaled: Mutex<bool>, cv: Condvar },
    Mutex { state: Mutex<(u32, u32)>, cv: Condvar }, // (tid proprietario, ricorsione)
    Thread(Arc<ThreadState>),
    #[allow(dead_code)] // distintivo per i thread AvRT ("Pro Audio"): nessun oggetto reale
    Avrt,
}

pub struct ThreadState {
    exit_code: Mutex<Option<u32>>,
    tid: AtomicU32,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

struct Obj {
    kind: Kind,
}

static OBJECTS: LazyLock<Mutex<HashMap<u64, Arc<Obj>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static NAMES: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_HANDLE: AtomicU32 = AtomicU32::new(1);

/// Thread vivi per tid: serve a ExitThread per trovare il proprio stato.
pub static THREADS: LazyLock<Mutex<HashMap<u32, Arc<ThreadState>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn register(kind: Kind) -> u64 {
    let h = HANDLE_BASE + 8 * NEXT_HANDLE.fetch_add(1, Ordering::SeqCst) as u64;
    OBJECTS.lock().unwrap().insert(h, Arc::new(Obj { kind }));
    h
}

/// Un handle nuovo non associato ad alcun oggetto (lo usano i file mapping, che hanno
/// la propria tabella in `mm`).
pub fn next_handle() -> u64 {
    HANDLE_BASE + 8 * NEXT_HANDLE.fetch_add(1, Ordering::SeqCst) as u64
}

fn get(h: u64) -> Option<Arc<Obj>> {
    if h == 0 || h == 0xFFFF_FFFF_FFFF_FFFF {
        return None;
    }
    OBJECTS.lock().unwrap().get(&h).cloned()
}

fn wide_string(ptr: *const u16) -> String {
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

// ---- Eventi ----

pub(crate) unsafe extern "win64" fn create_event_w(_sa: *const c_void, manual: i32, initial: i32, name: *const u16) -> u64 {
    let name = wide_string(name);
    if !name.is_empty() {
        let existing = NAMES.lock().unwrap().get(&name).copied();
        if let Some(h) = existing {
            return h;
        }
    }
    let h = register(Kind::Event { manual: manual != 0, signaled: Mutex::new(initial != 0), cv: Condvar::new() });
    if !name.is_empty() {
        NAMES.lock().unwrap().insert(name, h);
    }
    h
}

pub(crate) unsafe extern "win64" fn open_event_w(_access: u32, _inherit: i32, name: *const u16) -> u64 {
    let name = wide_string(name);
    NAMES.lock().unwrap().get(&name).copied().unwrap_or_else(|| {
        teb::set_last_error(ERROR_NOT_FOUND);
        0
    })
}

fn event_signal(h: u64, to: bool) -> i32 {
    match get(h) {
        Some(obj) => match &obj.kind {
            Kind::Event { signaled, cv, .. } => {
                let mut s = signaled.lock().unwrap();
                *s = to;
                cv.notify_all();
                1
            }
            _ => {
                teb::set_last_error(6 /* ERROR_INVALID_HANDLE */);
                0
            }
        },
        None => 0,
    }
}

pub(crate) unsafe extern "win64" fn set_event(h: u64) -> i32 {
    event_signal(h, true)
}

pub(crate) unsafe extern "win64" fn reset_event(h: u64) -> i32 {
    event_signal(h, false)
}

pub(crate) unsafe extern "win64" fn pulse_event(h: u64) -> i32 {
    event_signal(h, true);
    event_signal(h, false);
    1
}

// ---- Mutex ----

pub(crate) unsafe extern "win64" fn create_mutex_w(_sa: *const c_void, initial: i32, name: *const u16) -> u64 {
    let name = wide_string(name);
    if !name.is_empty() {
        let existing = NAMES.lock().unwrap().get(&name).copied();
        if let Some(h) = existing {
            return h;
        }
    }
    let tid = if initial != 0 { teb::gettid_win() } else { 0 };
    let h = register(Kind::Mutex { state: Mutex::new((tid, i32::from(initial != 0) as u32)), cv: Condvar::new() });
    if !name.is_empty() {
        NAMES.lock().unwrap().insert(name, h);
    }
    h
}

pub(crate) unsafe extern "win64" fn open_mutex_w(_access: u32, _inherit: i32, name: *const u16) -> u64 {
    let name = wide_string(name);
    NAMES.lock().unwrap().get(&name).copied().unwrap_or_else(|| {
        teb::set_last_error(ERROR_NOT_FOUND);
        0
    })
}

pub(crate) unsafe extern "win64" fn release_mutex(h: u64) -> i32 {
    match get(h) {
        Some(obj) => match &obj.kind {
            Kind::Mutex { state, cv } => {
                let tid = teb::gettid_win();
                let mut s = state.lock().unwrap();
                if s.0 != tid {
                    teb::set_last_error(288 /* ERROR_NOT_OWNER */);
                    return 0;
                }
                s.1 = s.1.saturating_sub(1);
                if s.1 == 0 {
                    s.0 = 0;
                    cv.notify_all();
                }
                1
            }
            _ => 0,
        },
        None => 0,
    }
}

// ---- Thread ----

pub(crate) unsafe extern "win64" fn create_thread(
    _attrs: *const c_void,
    _stack: usize,
    start: *mut c_void,
    param: *mut c_void,
    _flags: u32,
    tid_out: *mut u32,
) -> u64 {
    if start.is_null() {
        teb::set_last_error(87);
        return 0;
    }
    let state = Arc::new(ThreadState { exit_code: Mutex::new(None), tid: AtomicU32::new(0), join: Mutex::new(None) });
    let state2 = state.clone();
    // Il tid lo conosce solo il thread nuovo: ce lo passa prima di eseguire il modulo.
    let (tx, rx) = std::sync::mpsc::channel::<u32>();
    let builder = std::thread::Builder::new().stack_size(4 * 1024 * 1024);
    let start_addr = start as usize;
    let param_addr = param as usize;
    let spawned = builder.spawn(move || {
        let start = start_addr as *mut c_void;
        let param = param_addr as *mut c_void;
        // TEB dedicato prima di qualsiasi codice del modulo: il GS base ereditato punta
        // al TEB del thread padre e andrebbe a leggere i suoi TLS.
        teb::install();
        let tid = teb::gettid_win();
        state2.tid.store(tid, Ordering::SeqCst);
        THREADS.lock().unwrap().insert(tid, state2.clone());
        let _ = tx.send(tid);
        let f: unsafe extern "win64" fn(*mut c_void) -> u32 = unsafe { std::mem::transmute(start) };
        let code = unsafe { f(param) };
        thread_finished(&state2, code);
        // il ritorno naturale termina il thread
    });
    match spawned {
        Ok(handle) => {
            *state.join.lock().unwrap() = Some(handle);
            if !tid_out.is_null() {
                if let Ok(tid) = rx.recv_timeout(Duration::from_millis(1000)) {
                    unsafe { tid_out.write(tid) };
                }
            }
            register(Kind::Thread(state))
        }
        Err(_) => {
            teb::set_last_error(8);
            0
        }
    }
}

/// Esito di un thread: codice di uscita, rimozione dal registro, callback FLS.
pub fn thread_finished(state: &Arc<ThreadState>, code: u32) {
    *state.exit_code.lock().unwrap() = Some(code);
    THREADS.lock().unwrap().remove(&state.tid.load(Ordering::SeqCst));
    teb::run_thread_detach();
}

pub(crate) unsafe extern "win64" fn exit_thread(code: u32) -> ! {
    let tid = teb::gettid_win();
    if let Some(state) = THREADS.lock().unwrap().get(&tid).cloned() {
        thread_finished(&state, code);
    } else {
        teb::run_thread_detach();
    }
    unsafe { libc::pthread_exit(std::ptr::null_mut()) };
}

pub(crate) unsafe extern "win64" fn get_exit_code_thread(h: u64, out: *mut u32) -> i32 {
    match get(h) {
        Some(obj) => match &obj.kind {
            Kind::Thread(state) => {
                let code = state.exit_code.lock().unwrap().unwrap_or(STILL_ACTIVE);
                if !out.is_null() {
                    unsafe { out.write(code) };
                }
                1
            }
            _ => 0,
        },
        None => 0,
    }
}

pub fn thread_exit_code(state: &Arc<ThreadState>) -> Option<u32> {
    *state.exit_code.lock().unwrap()
}

pub(crate) unsafe extern "win64" fn free_library_and_exit_thread(_hmod: *mut c_void, code: u32) -> ! {
    unsafe { exit_thread(code) }
}

// ---- Attese ----

/// Attende un oggetto con la sua condvar; `consume` vale per eventi a reset automatico.
fn wait_event(obj: &Obj, timeout: Option<Duration>) -> u32 {
    match &obj.kind {
        Kind::Event { manual, signaled, cv } => {
            let mut s = signaled.lock().unwrap();
            let deadline = timeout.map(|t| Instant::now() + t);
            loop {
                if *s {
                    if !*manual {
                        *s = false;
                    }
                    return WAIT_OBJECT_0;
                }
                match deadline {
                    Some(d) => {
                        let now = Instant::now();
                        if now >= d {
                            return WAIT_TIMEOUT;
                        }
                        let (guard, _) = cv.wait_timeout(s, d - now).unwrap();
                        s = guard;
                    }
                    None => s = cv.wait(s).unwrap(),
                }
            }
        }
        Kind::Mutex { state, cv } => {
            let tid = teb::gettid_win();
            let mut s = state.lock().unwrap();
            let deadline = timeout.map(|t| Instant::now() + t);
            loop {
                if s.0 == 0 {
                    *s = (tid, 1);
                    return WAIT_OBJECT_0;
                }
                if s.0 == tid {
                    s.1 += 1;
                    return WAIT_OBJECT_0;
                }
                match deadline {
                    Some(d) => {
                        let now = Instant::now();
                        if now >= d {
                            return WAIT_TIMEOUT;
                        }
                        let (guard, _) = cv.wait_timeout(s, d - now).unwrap();
                        s = guard;
                    }
                    None => s = cv.wait(s).unwrap(),
                }
            }
        }
        Kind::Thread(state) => {
            // Attende per polling del codice di uscita: il thread finisce per conto suo
            // (ritorno naturale o ExitThread/pthread_exit).
            let deadline = timeout.map(|t| Instant::now() + t);
            loop {
                if state.exit_code.lock().unwrap().is_some() {
                    return WAIT_OBJECT_0;
                }
                if let Some(d) = deadline {
                    if Instant::now() >= d {
                        return WAIT_TIMEOUT;
                    }
                }
                std::thread::sleep(Duration::from_micros(250));
            }
        }
        Kind::Avrt => {
            teb::set_last_error(6);
            WAIT_FAILED
        }
    }
}

/// Un handle è "pronto" senza consumarlo (per WaitForMultipleObjects).
fn peek_ready(obj: &Obj) -> bool {
    match &obj.kind {
        Kind::Event { signaled, .. } => *signaled.lock().unwrap(),
        Kind::Mutex { state, .. } => state.lock().unwrap().0 == 0 || state.lock().unwrap().0 == teb::gettid_win(),
        Kind::Thread(state) => state.exit_code.lock().unwrap().is_some(),
        Kind::Avrt => false,
    }
}

pub(crate) unsafe extern "win64" fn wait_for_single_object(h: u64, timeout_ms: u32) -> u32 {
    match get(h) {
        Some(obj) => {
            let t = if timeout_ms == INFINITE { None } else { Some(Duration::from_millis(timeout_ms as u64)) };
            wait_event(&obj, t)
        }
        None => {
            teb::set_last_error(6);
            WAIT_FAILED
        }
    }
}

pub(crate) unsafe extern "win64" fn wait_for_single_object_ex(h: u64, timeout_ms: u32, _alertable: i32) -> u32 {
    unsafe { wait_for_single_object(h, timeout_ms) }
}

pub(crate) unsafe extern "win64" fn wait_for_multiple_objects(count: u32, handles: *const u64, wait_all: i32, timeout_ms: u32) -> u32 {
    let n = count as usize;
    let hs: Vec<u64> = unsafe { std::slice::from_raw_parts(handles, n) }.to_vec();
    let objs: Option<Vec<Arc<Obj>>> = hs.iter().map(|&h| get(h)).collect();
    let Some(objs) = objs else {
        teb::set_last_error(6);
        return WAIT_FAILED;
    };
    let deadline = if timeout_ms == INFINITE {
        None
    } else {
        Some(Instant::now() + Duration::from_millis(timeout_ms as u64))
    };
    loop {
        if wait_all != 0 {
            if objs.iter().all(|o| peek_ready(o)) {
                // consuma gli eventi a reset automatico e i mutex
                for o in &objs {
                    let _ = wait_event(o, Some(Duration::ZERO));
                }
                return WAIT_OBJECT_0;
            }
        } else if let Some((i, o)) = objs.iter().enumerate().find(|(_, o)| peek_ready(o)) {
            let _ = wait_event(o, Some(Duration::ZERO));
            return WAIT_OBJECT_0 + i as u32;
        }
        if let Some(d) = deadline {
            if Instant::now() >= d {
                return WAIT_TIMEOUT;
            }
        }
        std::thread::sleep(Duration::from_micros(250));
    }
}

pub(crate) unsafe extern "win64" fn close_handle(h: u64) -> i32 {
    if h == STD_INPUT || h == STD_OUTPUT || h == STD_ERROR {
        return 1;
    }
    OBJECTS.lock().unwrap().remove(&h).is_some() as i32
}
