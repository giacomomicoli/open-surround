//! Sincronizzazione: SRW lock, critical section e condition variable sono strutture *incorporate*
//! nella memoria del modulo (il modulo le alloca nei suoi oggetti e ci passa i puntatori), quindi
//! le implementiamo con un protocollo nostro su quei byte, più un registro di attesa basato su
//! park/unpark per chi resta in coda. InitOnceExecuteOnce segue lo stesso schema.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Slot di attesa: il risvegliante alza il flag e la condvar sblocca il thread in attesa.
struct WaitSlot {
    done: Mutex<bool>,
    cv: Condvar,
}

static WAITERS: LazyLock<Mutex<HashMap<usize, Vec<Arc<WaitSlot>>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Mette in coda il thread corrente su `key`. `recheck` (chiamata con il registro bloccato)
/// ritorna true se prima della registrazione è arrivato un risveglio "perso": in tal caso non
/// ci si parcheggia affatto. Altrimenti attende il risveglio e torna (il chiamante ritenta
/// l'acquisizione: servono i recheck di entrambi i lati per non perdere risvegli).
fn park_on(key: usize, recheck: impl Fn() -> bool) {
    let slot = Arc::new(WaitSlot { done: Mutex::new(false), cv: Condvar::new() });
    {
        let mut reg = WAITERS.lock().unwrap();
        if recheck() {
            return;
        }
        reg.entry(key).or_default().push(slot.clone());
    }
    let mut done = slot.done.lock().unwrap();
    while !*done {
        done = slot.cv.wait(done).unwrap();
    }
}

/// Come `park_on` ma con timeout; ritorna true se risvegliati entro il tempo, false al timeout
/// (in entrambi i casi l'attesa non va persa: si esce solo con il flag alzato o con il tempo scaduto).
fn park_timeout(key: usize, recheck: impl Fn() -> bool, timeout: Option<Duration>) -> bool {
    if let Some(t) = timeout {
        if t.is_zero() {
            return recheck();
        }
    }
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        let slot = Arc::new(WaitSlot { done: Mutex::new(false), cv: Condvar::new() });
        {
            let mut reg = WAITERS.lock().unwrap();
            if recheck() {
                return true;
            }
            reg.entry(key).or_default().push(slot.clone());
        }
        let mut done = slot.done.lock().unwrap();
        while !*done {
            match deadline {
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        // Togliersi dalla coda prima di uscire.
                        drop(done);
                        WAITERS.lock().unwrap().get_mut(&key).map(|v| v.retain(|s| !Arc::ptr_eq(s, &slot)));
                        return false;
                    }
                    let (guard, _res) = slot.cv.wait_timeout(done, d - now).unwrap();
                    done = guard;
                }
                None => done = slot.cv.wait(done).unwrap(),
            }
        }
        return true;
    }
}

/// Risveglia `all` (o uno solo) dei thread in coda su `key`.
fn wake(key: usize, all: bool) {
    let mut reg = WAITERS.lock().unwrap();
    if let Some(queue) = reg.get_mut(&key) {
        if all {
            for slot in queue.drain(..) {
                *slot.done.lock().unwrap() = true;
                slot.cv.notify_one();
            }
        } else if let Some(slot) = queue.pop() {
            *slot.done.lock().unwrap() = true;
            slot.cv.notify_one();
        }
    }
}

// ---- SRW lock (8 byte nella memoria del modulo) ----

/// Bit 63 = posseduto in esclusiva; i bit bassi = lettori condivisi.
const EXCL: u64 = 1 << 63;

unsafe fn srw_ptr(l: *mut c_void) -> *mut AtomicU64 {
    debug_assert!(!l.is_null());
    l as *mut AtomicU64
}

pub(crate) unsafe extern "win64" fn acquire_srw_exclusive(l: *mut c_void) {
    let a = unsafe { srw_ptr(l) };
    if unsafe { (*a).load(Ordering::Relaxed) } == 0 {
        if unsafe { (*a).compare_exchange_weak(0, EXCL, Ordering::Acquire, Ordering::Relaxed) }.is_ok() {
            return;
        }
    }
    unsafe { park_on(l as usize, || (*a).load(Ordering::Acquire) == 0) };
    loop {
        if unsafe { (*a).compare_exchange(0, EXCL, Ordering::Acquire, Ordering::Relaxed) }.is_ok() {
            return;
        }
        unsafe { park_on(l as usize, || (*a).load(Ordering::Acquire) == 0) };
    }
}

pub(crate) unsafe extern "win64" fn try_acquire_srw_exclusive(l: *mut c_void) -> i32 {
    let a = unsafe { srw_ptr(l) };
    if unsafe { (*a).compare_exchange(0, EXCL, Ordering::Acquire, Ordering::Relaxed) }.is_ok() {
        1
    } else {
        0
    }
}

pub(crate) unsafe extern "win64" fn release_srw_exclusive(l: *mut c_void) {
    let a = unsafe { srw_ptr(l) };
    unsafe { (*a).store(0, Ordering::Release) };
    wake(l as usize, true);
}

pub(crate) unsafe extern "win64" fn acquire_srw_shared(l: *mut c_void) {
    let a = unsafe { srw_ptr(l) };
    loop {
        let cur = unsafe { (*a).load(Ordering::Acquire) };
        if cur & EXCL == 0 && unsafe { (*a).compare_exchange_weak(cur, cur + 1, Ordering::Acquire, Ordering::Relaxed) }.is_ok() {
            return;
        }
        unsafe { park_on(l as usize, || (*a).load(Ordering::Acquire) & EXCL == 0) };
    }
}

pub(crate) unsafe extern "win64" fn release_srw_shared(l: *mut c_void) {
    let a = unsafe { srw_ptr(l) };
    if unsafe { (*a).fetch_sub(1, Ordering::Release) } == 1 {
        wake(l as usize, true);
    }
}

// ---- Critical section (40 byte nella memoria del modulo) ----

// Protocollo nostro sui byte del modulo (nessuno all'infuori di questi shim li tocca):
// +8: tid del proprietario (0 = libera), +16: ricorsione, +32: spin count.

unsafe fn cs_state(cs: *mut c_void) -> *mut AtomicU64 {
    (cs as *mut u8).add(8) as *mut AtomicU64
}
unsafe fn cs_rec(cs: *mut c_void) -> *mut u64 {
    (cs as *mut u8).add(16) as *mut u64
}

pub(crate) unsafe extern "win64" fn initialize_critical_section(cs: *mut c_void) {
    unsafe { std::ptr::write_bytes(cs as *mut u8, 0, 40) };
}

pub(crate) unsafe extern "win64" fn initialize_critical_section_and_spin_count(cs: *mut c_void, spin: u32) -> i32 {
    unsafe { initialize_critical_section(cs) };
    unsafe { *((cs as *mut u8).add(32) as *mut u32) = spin };
    1
}

pub(crate) unsafe extern "win64" fn initialize_critical_section_ex(cs: *mut c_void, _flags: u32, spin: u32) -> i32 {
    unsafe { initialize_critical_section_and_spin_count(cs, spin) }
}

pub(crate) unsafe extern "win64" fn enter_critical_section(cs: *mut c_void) {
    let state = unsafe { cs_state(cs) };
    let tid = crate::teb::gettid_win() as u64;
    loop {
        let cur = unsafe { (*state).load(Ordering::Relaxed) };
        if cur == 0 {
            if unsafe { (*state).compare_exchange_weak(0, tid, Ordering::Acquire, Ordering::Relaxed) }.is_ok() {
                unsafe { *cs_rec(cs) = 1 };
                return;
            }
        } else if cur == tid {
            unsafe { *cs_rec(cs) += 1 };
            return;
        }
        unsafe { park_on(cs as usize, || { let s = (*state).load(Ordering::Acquire); s == 0 || s == tid }) };
    }
}

pub(crate) unsafe extern "win64" fn try_enter_critical_section(cs: *mut c_void) -> i32 {
    let state = unsafe { cs_state(cs) };
    let tid = crate::teb::gettid_win() as u64;
    let cur = unsafe { (*state).load(Ordering::Relaxed) };
    if cur == 0 && unsafe { (*state).compare_exchange(0, tid, Ordering::Acquire, Ordering::Relaxed) }.is_ok() {
        unsafe { *cs_rec(cs) = 1 };
        1
    } else if cur == tid {
        unsafe { *cs_rec(cs) += 1 };
        1
    } else {
        0
    }
}

pub(crate) unsafe extern "win64" fn leave_critical_section(cs: *mut c_void) {
    let state = unsafe { cs_state(cs) };
    let tid = crate::teb::gettid_win() as u64;
    if unsafe { (*state).load(Ordering::Relaxed) } != tid {
        return; // il modulo non dovrebbe mai farlo; meglio ignorare che abortire
    }
    let rec = unsafe { *cs_rec(cs) };
    if rec <= 1 {
        unsafe { (*state).store(0, Ordering::Release) };
        wake(cs as usize, false);
    } else {
        unsafe { *cs_rec(cs) = rec - 1 };
    }
}

pub(crate) unsafe extern "win64" fn delete_critical_section(cs: *mut c_void) {
    unsafe { std::ptr::write_bytes(cs as *mut u8, 0, 40) };
}

// ---- Condition variable (8 byte, indirizzo usato come chiave) ----

pub(crate) unsafe extern "win64" fn sleep_condition_variable_srw(cv: *mut c_void, srw: *mut c_void, timeout_ms: u32, flags: u32) -> i32 {
    // CONDITION_VARIABLE_LOCKMODE_SHARED = 1
    let shared = flags & 1 != 0;
    if shared {
        unsafe { release_srw_shared(srw) };
    } else {
        unsafe { release_srw_exclusive(srw) };
    }
    let timeout = if timeout_ms == 0xFFFF_FFFF { None } else { Some(Duration::from_millis(timeout_ms as u64)) };
    let signaled = park_timeout(cv as usize, || false, timeout);
    if shared {
        unsafe { acquire_srw_shared(srw) };
    } else {
        unsafe { acquire_srw_exclusive(srw) };
    }
    // Al timeout Windows ritorna 0 con ERROR_TIMEOUT; qui segnaliamo l'esito.
    if !signaled {
        crate::teb::set_last_error(1460 /* ERROR_TIMEOUT */);
    }
    1
}

pub(crate) unsafe extern "win64" fn wake_condition_variable(cv: *mut c_void) {
    wake(cv as usize, false);
}

pub(crate) unsafe extern "win64" fn wake_all_condition_variable(cv: *mut c_void) {
    wake(cv as usize, true);
}

// ---- InitOnceExecuteOnce (8 byte: 0 = nuovo, 1 = in corso, 2 = fatto) ----

pub(crate) unsafe extern "win64" fn init_once_execute_once(
    once: *mut c_void,
    cb: Option<unsafe extern "win64" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32>,
    param: *mut c_void,
    context: *mut *mut c_void,
) -> i32 {
    let state = once as *mut AtomicU64;
    let cb = match cb {
        Some(cb) => cb,
        None => return 0,
    };
    loop {
        match unsafe { (*state).compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed) } {
            Ok(_) => {
                // `context` può essere NULL: il callback riceve comunque il suo slot.
                let mut ctx: *mut c_void = if context.is_null() { std::ptr::null_mut() } else { unsafe { context.read() } };
                let ctx_arg: *mut *mut c_void = if context.is_null() { std::ptr::null_mut() } else { &mut ctx };
                let r = unsafe { cb(once, param, ctx_arg) };
                if !context.is_null() {
                    unsafe { context.write(ctx) };
                }
                unsafe { (*state).store(2, Ordering::Release) };
                wake(once as usize, true);
                return r;
            }
            Err(cur) if cur == 2 => return 1,
            Err(_) => {
                unsafe { park_on(once as usize, || (*state).load(Ordering::Acquire) != 1) };
                if unsafe { (*state).load(Ordering::Acquire) } == 2 {
                    return 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn waiters_wake_one() {
        let key = 0x1234_5678usize;
        let hit = Arc::new(AtomicBool::new(false));
        let h = hit.clone();
        let t = std::thread::spawn(move || {
            park_on(key, || false); // recheck falso: si registra e parcheggia
            h.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(!hit.load(Ordering::SeqCst), "il thread deve essere ancora in attesa");
        wake(key, false);
        t.join().unwrap();
        assert!(hit.load(Ordering::SeqCst), "il thread deve essere stato svegliato");
    }

    #[test]
    fn waiters_timeout() {
        assert!(!park_timeout(0xdead_beef, || false, Some(Duration::from_millis(20))));
        assert!(park_timeout(0xdead_beef, || true, Some(Duration::from_millis(20))));
    }

    #[test]
    fn init_once_runs_once() {
        unsafe extern "win64" fn cb(_o: *mut c_void, p: *mut c_void, _c: *mut *mut c_void) -> i32 {
            *(p as *mut u32) += 1;
            1
        }
        let once = AtomicU64::new(0);
        let count = std::sync::atomic::AtomicU32::new(0);
        let ctx: *mut c_void = std::ptr::null_mut();
        unsafe {
            init_once_execute_once(&once as *const _ as *mut c_void, Some(cb), &count as *const _ as *mut c_void, &ctx as *const _ as *mut *mut c_void);
            init_once_execute_once(&once as *const _ as *mut c_void, Some(cb), &count as *const _ as *mut c_void, &ctx as *const _ as *mut *mut c_void);
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
