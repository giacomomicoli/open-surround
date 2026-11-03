//! ETW: il log interno del modulo va su un provider ({ebfc27e1-…}); senza una sessione ETW
//! resta spento anche su Windows. Con -v lo abilitiamo noi al massimo livello e stampiamo
//! gli EventWriteString, come fa l'host Windows con il suo hook IAT (doc sez. 12.8).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::teb;

pub static VERBOSE: AtomicBool = AtomicBool::new(false);
const FAKE_REG_HANDLE: u64 = 0x0e75_0000_0000_0001;

pub(crate) unsafe extern "win64" fn event_register(
    provider: *const u8,
    callback: Option<unsafe extern "win64" fn(*const u8, u32, u8, u64, u64, *const c_void, *mut c_void)>,
    context: *mut c_void,
    handle: *mut u64,
) -> u32 {
    if !handle.is_null() {
        unsafe { handle.write(FAKE_REG_HANDLE) };
    }
    if let Some(cb) = callback {
        if VERBOSE.load(Ordering::Relaxed) {
            // EVENT_CONTROL_CODE_ENABLE_PROVIDER = 1, livello 5 (verbose), tutte le keyword.
            unsafe { cb(provider, 1, 5, u64::MAX, 0, std::ptr::null(), context) };
        }
    }
    0
}

pub(crate) unsafe extern "win64" fn event_unregister(_handle: u64) -> u32 {
    0
}

pub(crate) unsafe extern "win64" fn event_write_string(_handle: u64, level: u8, _keyword: u64, text: *const u16) -> u32 {
    if VERBOSE.load(Ordering::Relaxed) && !text.is_null() {
        unsafe {
            let mut len = 0;
            while *text.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            eprintln!("[dap:{level}] {}", s.trim_end());
        }
    }
    let _ = teb::set_last_error;
    0
}
