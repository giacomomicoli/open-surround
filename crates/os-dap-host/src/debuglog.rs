//! Intercetta i messaggi `OutputDebugString` del modulo DAP senza debugger: su Windows (e Wine)
//! OutputDebugString solleva DBG_PRINTEXCEPTION_{C,WIDE_C}, che un handler vettoriale vede per primo.

use windows::Win32::Foundation::NTSTATUS;
use windows::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS};

const DBG_PRINTEXCEPTION_C: NTSTATUS = NTSTATUS(0x4001_0006);
const DBG_PRINTEXCEPTION_WIDE_C: NTSTATUS = NTSTATUS(0x4001_000A);
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    let rec = &*(*info).ExceptionRecord;
    let args = &rec.ExceptionInformation;
    let text = if rec.ExceptionCode == DBG_PRINTEXCEPTION_WIDE_C && rec.NumberParameters >= 2 {
        let len = (args[0] as usize).saturating_sub(1);
        String::from_utf16_lossy(std::slice::from_raw_parts(args[1] as *const u16, len))
    } else if rec.ExceptionCode == DBG_PRINTEXCEPTION_C && rec.NumberParameters >= 2 {
        let len = (args[0] as usize).saturating_sub(1);
        String::from_utf8_lossy(std::slice::from_raw_parts(args[1] as *const u8, len)).into_owned()
    } else {
        return EXCEPTION_CONTINUE_SEARCH;
    };
    eprintln!("[dap] {}", text.trim_end());
    EXCEPTION_CONTINUE_SEARCH
}

pub fn install() {
    unsafe { AddVectoredExceptionHandler(1, Some(handler)) };
}
