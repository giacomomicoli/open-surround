//! RTWorkQ: RtwqCreateAsyncResult e RtwqPutWorkItem, le due funzioni della coda di lavoro
//! real-time che il modulo importa (doc sez. 12.12, punto 1).
//!
//! Interfacce (windows-0.62.2, System/Threading/mod.rs):
//! - `IRtwqAsyncCallback` {a27003cf-2354-4f2a-8d6a-ab7cff15437e}: IUnknown (0-2),
//!   `GetParameters(DWORD*, DWORD*)` (3), `Invoke(IRtwqAsyncResult*)` (4).
//! - `IRtwqAsyncResult` {ac6b7889-0740-4d51-8619-905994a55cc6}: IUnknown (0-2), `GetState` (3),
//!   `GetStatus` (4), `SetStatus` (5), `GetObject` (6), `GetStateNoAddRef` (7); lo stesso
//!   ordine di `IMFAsyncResult`.
//!
//! Il risultato trattiene (AddRef) oggetto, callback e stato del chiamante e li rilascia quando
//! muore. `RtwqPutWorkItem` ritorna subito e il callback gira su un thread di lavoro con TEB
//! installato, come su Windows dove il lavoro passa a un thread della coda: chi chiama
//! PutWorkItem tenendo un lock che il callback riprende non va in deadlock. Un solo thread
//! serve tutte le code, quindi i work item girano in ordine di arrivo.

use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{LazyLock, Mutex};

use crate::shims::com::guid_bytes;

const S_OK: i32 = 0;
const E_POINTER: i32 = 0x8000_4003u32 as i32;
const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;
const E_UNEXPECTED: i32 = 0x8000_FFFFu32 as i32;

const IID_IUNKNOWN: u128 = 0x00000000_0000_0000_c000_000000000046;
pub const IID_IRTWQ_ASYNC_RESULT: u128 = 0xac6b7889_0740_4d51_8619_905994a55cc6;
/// Non serve al codice (il callback è del modulo): resta per i test e come riferimento.
#[allow(dead_code)]
pub const IID_IRTWQ_ASYNC_CALLBACK: u128 = 0xa27003cf_2354_4f2a_8d6a_ab7cff15437e;

/// Slot IUnknown di un oggetto COM qualunque (del modulo): AddRef = 1, Release = 2.
unsafe fn unknown_call(obj: *mut c_void, slot: usize) -> u32 {
    if obj.is_null() {
        return 0;
    }
    unsafe {
        let vtbl = *(obj as *const *const usize);
        let f: unsafe extern "win64" fn(*mut c_void) -> u32 = std::mem::transmute(*vtbl.add(slot));
        f(obj)
    }
}

unsafe fn add_ref(obj: *mut c_void) {
    unsafe { unknown_call(obj, 1) };
}

unsafe fn release(obj: *mut c_void) {
    unsafe { unknown_call(obj, 2) };
}

#[repr(C)]
struct AsyncResult {
    vtbl: &'static AsyncResultVtbl,
    refs: AtomicU32,
    /// I tre puntatori sono trattenuti (AddRef alla creazione, Release alla distruzione).
    app_object: *mut c_void,
    callback: *mut c_void,
    state: *mut c_void,
    status: AtomicI32,
}

unsafe fn this_of<'a>(this: *mut c_void) -> &'a AsyncResult {
    unsafe { &*(this as *const AsyncResult) }
}

unsafe extern "win64" fn ar_query_interface(this: *mut c_void, iid: *const u8, out: *mut *mut c_void) -> i32 {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    let iid = unsafe { &*(iid as *const [u8; 16]) };
    if *iid == guid_bytes(IID_IUNKNOWN) || *iid == guid_bytes(IID_IRTWQ_ASYNC_RESULT) {
        unsafe { ar_add_ref(this) };
        unsafe { out.write(this) };
        S_OK
    } else {
        unsafe { out.write(std::ptr::null_mut()) };
        E_NOINTERFACE
    }
}

unsafe extern "win64" fn ar_add_ref(this: *mut c_void) -> u32 {
    unsafe { this_of(this) }.refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "win64" fn ar_release(this: *mut c_void) -> u32 {
    let left = unsafe { this_of(this) }.refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if left == 0 {
        let r = unsafe { Box::from_raw(this as *mut AsyncResult) };
        unsafe {
            release(r.app_object);
            release(r.callback);
            release(r.state);
        }
    }
    left
}

/// Come IMFAsyncResult: E_POINTER se lo stato (o l'oggetto) non c'è.
unsafe fn out_add_ref(p: *mut c_void, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe { out.write(p) };
    if p.is_null() {
        return E_POINTER;
    }
    unsafe { add_ref(p) };
    S_OK
}

unsafe extern "win64" fn ar_get_state(this: *mut c_void, out: *mut *mut c_void) -> i32 {
    unsafe { out_add_ref(this_of(this).state, out) }
}

unsafe extern "win64" fn ar_get_status(this: *mut c_void) -> i32 {
    unsafe { this_of(this) }.status.load(Ordering::Acquire)
}

unsafe extern "win64" fn ar_set_status(this: *mut c_void, hr: i32) -> i32 {
    unsafe { this_of(this) }.status.store(hr, Ordering::Release);
    S_OK
}

unsafe extern "win64" fn ar_get_object(this: *mut c_void, out: *mut *mut c_void) -> i32 {
    unsafe { out_add_ref(this_of(this).app_object, out) }
}

unsafe extern "win64" fn ar_get_state_no_add_ref(this: *mut c_void) -> *mut c_void {
    unsafe { this_of(this) }.state
}

#[repr(C)]
struct AsyncResultVtbl {
    query_interface: unsafe extern "win64" fn(*mut c_void, *const u8, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "win64" fn(*mut c_void) -> u32,
    release: unsafe extern "win64" fn(*mut c_void) -> u32,
    get_state: unsafe extern "win64" fn(*mut c_void, *mut *mut c_void) -> i32,
    get_status: unsafe extern "win64" fn(*mut c_void) -> i32,
    set_status: unsafe extern "win64" fn(*mut c_void, i32) -> i32,
    get_object: unsafe extern "win64" fn(*mut c_void, *mut *mut c_void) -> i32,
    get_state_no_add_ref: unsafe extern "win64" fn(*mut c_void) -> *mut c_void,
}

static ASYNC_RESULT_VTABLE: AsyncResultVtbl = AsyncResultVtbl {
    query_interface: ar_query_interface,
    add_ref: ar_add_ref,
    release: ar_release,
    get_state: ar_get_state,
    get_status: ar_get_status,
    set_status: ar_set_status,
    get_object: ar_get_object,
    get_state_no_add_ref: ar_get_state_no_add_ref,
};

/// `HRESULT RtwqCreateAsyncResult(IUnknown* appObject, IRtwqAsyncCallback* callback,
/// IUnknown* appState, IRtwqAsyncResult** asyncResult)`.
pub(crate) unsafe extern "win64" fn rtwq_create_async_result(
    app_object: *mut c_void,
    callback: *mut c_void,
    app_state: *mut c_void,
    out: *mut *mut c_void,
) -> i32 {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe {
        add_ref(app_object);
        add_ref(callback);
        add_ref(app_state);
    }
    let r = Box::new(AsyncResult {
        vtbl: &ASYNC_RESULT_VTABLE,
        refs: AtomicU32::new(1),
        app_object,
        callback,
        state: app_state,
        status: AtomicI32::new(S_OK),
    });
    unsafe { out.write(Box::into_raw(r) as *mut c_void) };
    S_OK
}

/// Coda del thread di lavoro: indirizzi di IRtwqAsyncResult già trattenuti (AddRef).
static WORKER: LazyLock<Mutex<Option<Sender<usize>>>> = LazyLock::new(|| Mutex::new(None));

fn worker() -> Option<Sender<usize>> {
    let mut w = WORKER.lock().unwrap();
    if w.is_none() {
        let (tx, rx) = channel::<usize>();
        let spawned = std::thread::Builder::new().name("rtwq-worker".into()).spawn(move || {
            // Il callback è codice del modulo: serve il TEB (il GS ereditato è del padre).
            crate::teb::install();
            crate::loader::notify_thread(crate::loader::DLL_THREAD_ATTACH);
            for result in rx {
                unsafe { invoke(result as *mut c_void) };
            }
        });
        if spawned.is_err() {
            return None;
        }
        *w = Some(tx);
    }
    w.clone()
}

/// Esegue il work item: `IRtwqAsyncCallback::Invoke` (slot 4) con il risultato, poi rilascia
/// il riferimento preso da PutWorkItem.
unsafe fn invoke(result: *mut c_void) {
    let callback = unsafe { this_of(result) }.callback;
    if !callback.is_null() {
        unsafe {
            let vtbl = *(callback as *const *const usize);
            let f: unsafe extern "win64" fn(*mut c_void, *mut c_void) -> i32 = std::mem::transmute(*vtbl.add(4));
            f(callback, result);
        }
    }
    unsafe { ar_release(result) };
}

/// `HRESULT RtwqPutWorkItem(DWORD queue, LONG priority, IRtwqAsyncResult* result)`: accoda e
/// ritorna; il callback gira sul thread di lavoro (coda e priorità non distinguono).
pub(crate) unsafe extern "win64" fn rtwq_put_work_item(_queue: u32, _priority: i32, result: *mut c_void) -> i32 {
    if result.is_null() {
        return E_POINTER;
    }
    if !std::ptr::eq(unsafe { this_of(result) }.vtbl, &ASYNC_RESULT_VTABLE) {
        // Un IRtwqAsyncResult non creato da RtwqCreateAsyncResult: non sappiamo trovarne il
        // callback (su Windows è lo stesso: la coda accetta solo i propri risultati).
        return E_UNEXPECTED;
    }
    unsafe { ar_add_ref(result) };
    match worker() {
        Some(tx) if tx.send(result as usize).is_ok() => S_OK,
        _ => {
            unsafe { ar_release(result) };
            E_UNEXPECTED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::time::{Duration, Instant};

    /// Oggetto IUnknown finto con conteggio dei riferimenti osservabile.
    #[repr(C)]
    struct Fake {
        vtbl: *const usize,
        refs: AtomicU32,
    }

    unsafe extern "win64" fn fake_qi(_this: *mut c_void, _iid: *const u8, _out: *mut *mut c_void) -> i32 {
        E_NOINTERFACE
    }
    unsafe extern "win64" fn fake_add_ref(this: *mut c_void) -> u32 {
        unsafe { &*(this as *const Fake) }.refs.fetch_add(1, Ordering::SeqCst) + 1
    }
    unsafe extern "win64" fn fake_release(this: *mut c_void) -> u32 {
        unsafe { &*(this as *const Fake) }.refs.fetch_sub(1, Ordering::SeqCst) - 1
    }

    static GET_PARAMETERS_CALLED: AtomicBool = AtomicBool::new(false);
    static INVOKED: AtomicBool = AtomicBool::new(false);
    /// Esiti dei controlli fatti dentro Invoke (bit per controllo).
    static CHECKS: AtomicU32 = AtomicU32::new(0);
    static STATE: AtomicUsize = AtomicUsize::new(0);
    static OBJECT: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "win64" fn cb_get_parameters(_this: *mut c_void, _flags: *mut u32, _queue: *mut u32) -> i32 {
        GET_PARAMETERS_CALLED.store(true, Ordering::SeqCst);
        E_UNEXPECTED
    }

    unsafe extern "win64" fn cb_invoke(_this: *mut c_void, result: *mut c_void) -> i32 {
        let mut ok = 0u32;
        unsafe {
            let vtbl = *(result as *const *const usize);
            let get_state: unsafe extern "win64" fn(*mut c_void, *mut *mut c_void) -> i32 = std::mem::transmute(*vtbl.add(3));
            let get_status: unsafe extern "win64" fn(*mut c_void) -> i32 = std::mem::transmute(*vtbl.add(4));
            let set_status: unsafe extern "win64" fn(*mut c_void, i32) -> i32 = std::mem::transmute(*vtbl.add(5));
            let get_object: unsafe extern "win64" fn(*mut c_void, *mut *mut c_void) -> i32 = std::mem::transmute(*vtbl.add(6));
            let no_add_ref: unsafe extern "win64" fn(*mut c_void) -> *mut c_void = std::mem::transmute(*vtbl.add(7));
            let mut p: *mut c_void = std::ptr::null_mut();
            if get_state(result, &mut p) == S_OK && p as usize == STATE.load(Ordering::SeqCst) {
                ok |= 1;
                fake_release(p);
            }
            if get_object(result, &mut p) == S_OK && p as usize == OBJECT.load(Ordering::SeqCst) {
                ok |= 2;
                fake_release(p);
            }
            if set_status(result, 0x8000_4005u32 as i32) == S_OK && get_status(result) == 0x8000_4005u32 as i32 {
                ok |= 4;
            }
            if no_add_ref(result) as usize == STATE.load(Ordering::SeqCst) {
                ok |= 8;
            }
            // il callback gira su un thread con TEB (il modulo legge gs:[0x58])
            if crate::teb::is_installed() {
                ok |= 16;
            }
        }
        CHECKS.store(ok, Ordering::SeqCst);
        INVOKED.store(true, Ordering::SeqCst);
        S_OK
    }

    #[test]
    fn put_work_item_invokes_callback() {
        let unk_vtbl: [usize; 3] = [fake_qi as usize, fake_add_ref as usize, fake_release as usize];
        let cb_vtbl: [usize; 5] =
            [fake_qi as usize, fake_add_ref as usize, fake_release as usize, cb_get_parameters as usize, cb_invoke as usize];
        let object = Fake { vtbl: unk_vtbl.as_ptr(), refs: AtomicU32::new(1) };
        let state = Fake { vtbl: unk_vtbl.as_ptr(), refs: AtomicU32::new(1) };
        let callback = Fake { vtbl: cb_vtbl.as_ptr(), refs: AtomicU32::new(1) };
        let p = |f: &Fake| f as *const Fake as *mut c_void;
        OBJECT.store(p(&object) as usize, Ordering::SeqCst);
        STATE.store(p(&state) as usize, Ordering::SeqCst);

        let mut result: *mut c_void = std::ptr::null_mut();
        unsafe {
            assert_eq!(rtwq_create_async_result(p(&object), p(&callback), p(&state), &mut result), S_OK);
            // QI ristretto: IRtwqAsyncResult sì, il callback no.
            let mut q: *mut c_void = std::ptr::null_mut();
            assert_eq!(ar_query_interface(result, guid_bytes(IID_IRTWQ_ASYNC_RESULT).as_ptr(), &mut q), S_OK);
            assert_eq!(q, result);
            ar_release(q);
            assert_eq!(ar_query_interface(result, guid_bytes(IID_IRTWQ_ASYNC_CALLBACK).as_ptr(), &mut q), E_NOINTERFACE);
            assert!(q.is_null());

            assert_eq!(rtwq_put_work_item(0, 0, result), S_OK);
            ar_release(result); // il chiamante lascia il suo riferimento: la coda tiene il proprio
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !INVOKED.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "Invoke non chiamato");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!GET_PARAMETERS_CALLED.load(Ordering::SeqCst), "chiamato GetParameters al posto di Invoke");
        assert_eq!(CHECKS.load(Ordering::SeqCst), 31, "GetState/GetObject/SetStatus/GetStateNoAddRef/TEB");
        // Dopo l'ultimo Release il risultato rilascia ciò che tratteneva.
        while callback.refs.load(Ordering::SeqCst) != 1 {
            assert!(Instant::now() < deadline, "il risultato non è stato liberato");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(object.refs.load(Ordering::SeqCst), 1);
        assert_eq!(state.refs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn missing_state_is_e_pointer() {
        let mut result: *mut c_void = std::ptr::null_mut();
        unsafe {
            assert_eq!(rtwq_create_async_result(std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), &mut result), S_OK);
            let mut p: *mut c_void = 1 as *mut c_void;
            assert_eq!(ar_get_state(result, &mut p), E_POINTER);
            assert!(p.is_null());
            assert_eq!(ar_get_object(result, &mut p), E_POINTER);
            assert_eq!(ar_get_status(result), S_OK);
            assert_eq!(ar_release(result), 0);
        }
    }
}
