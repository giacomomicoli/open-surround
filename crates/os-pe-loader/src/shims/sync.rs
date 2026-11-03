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

/// Come `park_on` ma con timeout; ritorna true se risvegliati entro il tempo, false al timeout.
/// (Le attese con timeout del modulo passano dalla CV, che usa `wait_slot` direttamente.)
#[cfg(test)]
fn park_timeout(key: usize, recheck: impl Fn() -> bool, timeout: Option<Duration>) -> bool {
    if timeout.is_some_and(|t| t.is_zero()) {
        return recheck();
    }
    let slot = Arc::new(WaitSlot { done: Mutex::new(false), cv: Condvar::new() });
    {
        let mut reg = WAITERS.lock().unwrap();
        if recheck() {
            return true;
        }
        reg.entry(key).or_default().push(slot.clone());
    }
    wait_slot(key, &slot, timeout.map(|t| Instant::now() + t))
}

/// Attende sul proprio slot (già in coda su `key`) fino al risveglio o a `deadline`. Al timeout
/// lo slot si toglie dalla coda; se non c'è più, un `wake` lo ha appena preso (e ha già alzato
/// il flag, con il registro bloccato): conta come risveglio, altrimenti andrebbe perso.
fn wait_slot(key: usize, slot: &Arc<WaitSlot>, deadline: Option<Instant>) -> bool {
    let mut done = slot.done.lock().unwrap();
    while !*done {
        match deadline {
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    drop(done);
                    let mut reg = WAITERS.lock().unwrap();
                    let queue = reg.get_mut(&key);
                    let was_queued = queue.is_some_and(|q| {
                        let before = q.len();
                        q.retain(|s| !Arc::ptr_eq(s, slot));
                        q.len() != before
                    });
                    return !was_queued;
                }
                done = slot.cv.wait_timeout(done, d - now).unwrap().0;
            }
            None => done = slot.cv.wait(done).unwrap(),
        }
    }
    true
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

/// SleepConditionVariableSRW: il thread si mette in coda sulla CV **prima** di rilasciare lo
/// SRW, quindi un Wake fatto da chi prende lo SRW subito dopo il rilascio lo trova (su
/// Windows rilascio e messa in coda sono atomici rispetto al risveglio). Al timeout: FALSE con
/// ERROR_TIMEOUT, come si aspetta `_Cnd_timedwait` del CRT MSVC.
pub(crate) unsafe extern "win64" fn sleep_condition_variable_srw(cv: *mut c_void, srw: *mut c_void, timeout_ms: u32, flags: u32) -> i32 {
    // CONDITION_VARIABLE_LOCKMODE_SHARED = 1
    let shared = flags & 1 != 0;
    let deadline = (timeout_ms != 0xFFFF_FFFF).then(|| Instant::now() + Duration::from_millis(timeout_ms as u64));
    let slot = Arc::new(WaitSlot { done: Mutex::new(false), cv: Condvar::new() });
    // Il registro va rilasciato prima di rilasciare lo SRW: anche il rilascio lo prende.
    WAITERS.lock().unwrap().entry(cv as usize).or_default().push(slot.clone());
    if shared {
        unsafe { release_srw_shared(srw) };
    } else {
        unsafe { release_srw_exclusive(srw) };
    }
    let signaled = wait_slot(cv as usize, &slot, deadline);
    if shared {
        unsafe { acquire_srw_shared(srw) };
    } else {
        unsafe { acquire_srw_exclusive(srw) };
    }
    if signaled {
        1
    } else {
        crate::teb::set_last_error(1460 /* ERROR_TIMEOUT */);
        0
    }
}

pub(crate) unsafe extern "win64" fn wake_condition_variable(cv: *mut c_void) {
    wake(cv as usize, false);
}

pub(crate) unsafe extern "win64" fn wake_all_condition_variable(cv: *mut c_void) {
    wake(cv as usize, true);
}

// ---- InitOnceExecuteOnce ----
//
// INIT_ONCE è un puntatore (8 byte): 0 = da fare, 1 = in corso, `ctx | 2` = fatto, con il
// contesto della callback nei bit alti (come Windows, che riserva i 2 bit bassi:
// INIT_ONCE_CTX_RESERVED_BITS).

const ONCE_IN_PROGRESS: u64 = 1;
const ONCE_DONE: u64 = 2;

/// `BOOL InitOnceExecuteOnce(INIT_ONCE*, PINIT_ONCE_FN, void* param, void** context)`.
/// La callback riceve `context` del chiamante (anche NULL); se restituisce FALSE l'INIT_ONCE
/// torna allo stato iniziale (un tentativo successivo la riesegue) e la funzione restituisce
/// FALSE; se riesce, il contesto viene conservato e restituito anche alle chiamate successive.
pub(crate) unsafe extern "win64" fn init_once_execute_once(
    once: *mut c_void,
    cb: Option<unsafe extern "win64" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32>,
    param: *mut c_void,
    context: *mut *mut c_void,
) -> i32 {
    let state = once as *mut AtomicU64;
    let done = |v: u64| {
        if !context.is_null() {
            unsafe { context.write((v & !3) as *mut c_void) };
        }
        1
    };
    loop {
        match unsafe { (*state).compare_exchange(0, ONCE_IN_PROGRESS, Ordering::Acquire, Ordering::Acquire) } {
            Ok(_) => {
                let ok = match cb {
                    Some(cb) => (unsafe { cb(once, param, context) }) != 0,
                    None => false,
                };
                let ctx = if ok && !context.is_null() { (unsafe { context.read() }) as u64 } else { 0 };
                unsafe { (*state).store(if ok { (ctx & !3) | ONCE_DONE } else { 0 }, Ordering::Release) };
                wake(once as usize, true);
                if !ok {
                    crate::teb::set_last_error(31 /* ERROR_GEN_FAILURE, come STATUS_UNSUCCESSFUL */);
                }
                return ok as i32;
            }
            Err(cur) if cur & 3 == ONCE_DONE => return done(cur),
            Err(_) => unsafe { park_on(once as usize, || (*state).load(Ordering::Acquire) != ONCE_IN_PROGRESS) },
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
    fn init_once_failure_retries_and_context() {
        static CALLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        unsafe extern "win64" fn cb(_o: *mut c_void, _p: *mut c_void, c: *mut *mut c_void) -> i32 {
            // il primo tentativo fallisce, il secondo riesce con un contesto
            if CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
                return 0;
            }
            *c = 0x1230 as *mut c_void;
            1
        }
        let once = AtomicU64::new(0);
        let o = &once as *const _ as *mut c_void;
        let mut ctx: *mut c_void = std::ptr::null_mut();
        unsafe {
            assert_eq!(init_once_execute_once(o, Some(cb), std::ptr::null_mut(), &mut ctx), 0);
            assert_eq!(once.load(Ordering::SeqCst), 0, "torna allo stato iniziale");
            assert_eq!(init_once_execute_once(o, Some(cb), std::ptr::null_mut(), &mut ctx), 1);
            assert_eq!(ctx as usize, 0x1230);
            let mut again: *mut c_void = std::ptr::null_mut();
            assert_eq!(init_once_execute_once(o, Some(cb), std::ptr::null_mut(), &mut again), 1);
            assert_eq!(again as usize, 0x1230, "contesto conservato");
        }
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
    }

    /// Produttore/consumatore sulla CV con attesa INFINITE: con il risveglio perso (rilascio
    /// dello SRW prima della messa in coda) prima o poi il consumatore restava fermo.
    #[test]
    fn condition_variable_no_lost_wakeup() {
        const N: u32 = 5000;
        struct Shared {
            srw: AtomicU64,
            full: AtomicU64,
            empty: AtomicU64,
            slot: std::cell::UnsafeCell<u32>, // protetto dallo SRW: 0 = vuoto
        }
        unsafe impl Sync for Shared {}
        let sh: &'static Shared =
            Box::leak(Box::new(Shared { srw: AtomicU64::new(0), full: AtomicU64::new(0), empty: AtomicU64::new(0), slot: 0.into() }));
        let p = |a: &AtomicU64| a as *const AtomicU64 as *mut c_void;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let consumer = std::thread::spawn(move || {
                let mut sum = 0u64;
                for _ in 0..N {
                    unsafe {
                        acquire_srw_exclusive(p(&sh.srw));
                        while *sh.slot.get() == 0 {
                            assert_eq!(sleep_condition_variable_srw(p(&sh.full), p(&sh.srw), 0xFFFF_FFFF, 0), 1);
                        }
                        sum += *sh.slot.get() as u64;
                        *sh.slot.get() = 0;
                        wake_condition_variable(p(&sh.empty));
                        release_srw_exclusive(p(&sh.srw));
                    }
                }
                sum
            });
            for i in 1..=N {
                unsafe {
                    acquire_srw_exclusive(p(&sh.srw));
                    while *sh.slot.get() != 0 {
                        assert_eq!(sleep_condition_variable_srw(p(&sh.empty), p(&sh.srw), 0xFFFF_FFFF, 0), 1);
                    }
                    *sh.slot.get() = i;
                    wake_condition_variable(p(&sh.full));
                    release_srw_exclusive(p(&sh.srw));
                }
            }
            let _ = tx.send(consumer.join().unwrap());
        });
        let sum = rx.recv_timeout(Duration::from_secs(30)).expect("produttore/consumatore bloccati: risveglio perso");
        assert_eq!(sum, (N as u64) * (N as u64 + 1) / 2);
    }

    #[test]
    fn condition_variable_timeout_returns_false() {
        crate::teb::install();
        let srw = AtomicU64::new(0);
        let cv = AtomicU64::new(0);
        let l = &srw as *const _ as *mut c_void;
        unsafe {
            acquire_srw_exclusive(l);
            crate::teb::set_last_error(0);
            let t0 = Instant::now();
            assert_eq!(sleep_condition_variable_srw(&cv as *const _ as *mut c_void, l, 10, 0), 0);
            assert!(t0.elapsed() >= Duration::from_millis(10));
            assert_eq!(crate::teb::last_error(), 1460);
            assert_eq!(srw.load(Ordering::SeqCst), EXCL, "lo SRW è di nuovo nostro");
            release_srw_exclusive(l);
        }
        assert!(WAITERS.lock().unwrap().get(&(&cv as *const _ as usize)).is_none_or(|q| q.is_empty()));
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
