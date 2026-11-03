//! Il resto del CRT statico MSVC: errori, tempo, localizzazione, conversioni di stringa,
//! ambiente, console, file (solo std handle), assert via MessageBox (stampati come fa
//! l'host Windows), eccezioni (abort informativo: l'unwinding x64 SEH non è implementato,
//! nel percorso offline verificato non serve), AVRT, RTWorkQ, SDDL.

use std::ffi::{c_void, CStr};

use crate::shims::etw::VERBOSE;
use crate::shims::handles::{STD_ERROR, STD_INPUT, STD_OUTPUT};
use crate::teb;

const S_OK: i32 = 0;
const TRUE: i32 = 1;
const INVALID_HANDLE_VALUE: u64 = 0xFFFF_FFFF_FFFF_FFFF;

// ---- errori e ultimo errore ----

pub(crate) unsafe extern "win64" fn get_last_error() -> u32 {
    teb::last_error()
}

pub(crate) unsafe extern "win64" fn set_last_error(e: u32) {
    teb::set_last_error(e);
}

// ---- tempo ----

pub(crate) unsafe extern "win64" fn query_performance_counter(out: *mut i64) -> i32 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    if !out.is_null() {
        unsafe { out.write(ts.tv_sec * 10_000_000 + ts.tv_nsec / 100) }; // unità 100 ns
    }
    TRUE
}

pub(crate) unsafe extern "win64" fn query_performance_frequency(out: *mut i64) -> i32 {
    if !out.is_null() {
        unsafe { out.write(10_000_000) };
    }
    TRUE
}

pub(crate) unsafe extern "win64" fn get_system_time_as_file_time(out: *mut u64) {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts) };
    let unix_100ns = (ts.tv_sec as u64) * 10_000_000 + (ts.tv_nsec as u64) / 100;
    // Da 1601-01-01 a 1970-01-01.
    if !out.is_null() {
        unsafe { out.write(unix_100ns + 116_444_736_000_000_000) };
    }
}

pub(crate) unsafe extern "win64" fn get_tick_count_64() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as u64) * 1000 + (ts.tv_nsec as u64) / 1_000_000
}

// ---- identificazione processo/thread ----

pub(crate) unsafe extern "win64" fn get_current_thread_id() -> u32 {
    teb::gettid_win()
}

pub(crate) unsafe extern "win64" fn get_current_process_id() -> u32 {
    unsafe { libc::getpid() as u32 }
}

pub(crate) unsafe extern "win64" fn get_current_process() -> *mut c_void {
    0xFFFF_FFFF_FFFF_FFFFusize as *mut c_void
}

pub(crate) unsafe extern "win64" fn is_debugger_present() -> i32 {
    0
}

pub(crate) unsafe extern "win64" fn is_processor_feature_present(feature: u32) -> i32 {
    // Le feature x86-64 che il CRT MSVC può chiedere ci sono tutte su questa macchina.
    matches!(feature, 0..=36).into()
}

// ---- exception / terminazione ----

/// RaiseException è il veicolo delle eccezioni C++ MSVC e dei percorsi fatali del CRT.
/// L'unwinding SEH x64 non è implementato: nessun percorso verificato lo attraversa,
/// quindi terminiamo con un messaggio chiaro invece di un crash obscuro.
pub(crate) unsafe extern "win64" fn raise_exception(code: u32, flags: u32, count: u32, args: *mut usize) -> ! {
    eprintln!(
        "[os-pe-loader] RaiseException({code:#x}, flags {flags:#x}, {count} argomenti): \
         eccezioni SEH non supportate; percorso C++/CRT non previsto nel flusso offline."
    );
    if !args.is_null() && count > 0 {
        eprintln!("[os-pe-loader] primo argomento: {:#x}", unsafe { *args });
    }
    std::process::abort();
}

pub(crate) unsafe extern "win64" fn rtl_capture_context(_ctx: *mut c_void) -> ! {
    eprintln!("[os-pe-loader] RtlCaptureContext: contesti SEH non supportati.");
    std::process::abort();
}

pub(crate) unsafe extern "win64" fn rtl_lookup_function_entry(
    _pc: u64,
    _base: *mut u64,
    _history: *mut c_void,
) -> *mut c_void {
    std::ptr::null_mut()
}

pub(crate) unsafe extern "win64" fn rtl_virtual_unwind(
    _handler: u32,
    _base: u64,
    _pc: u64,
    _ctx: *mut c_void,
    _entry: *mut c_void,
    _frame: *mut *mut c_void,
    _ctx_out: *mut c_void,
) -> i32 {
    eprintln!("[os-pe-loader] RtlVirtualUnwind: unwinding SEH non supportato.");
    std::process::abort();
}

pub(crate) unsafe extern "win64" fn rtl_unwind_ex(
    _frame: *mut c_void,
    _pc: u64,
    _ctx: *mut c_void,
    _hist: *mut c_void,
    _frame2: *mut *mut c_void,
    _code: u32,
) -> ! {
    eprintln!("[os-pe-loader] RtlUnwindEx: unwinding SEH non supportato.");
    std::process::abort();
}

/// RtlPcToFileHeader: serve al CRT per stabilire il modulo di un indirizzo.
pub(crate) unsafe extern "win64" fn rtl_pc_to_file_header(pc: *const c_void, base_out: *mut *mut c_void) -> *mut c_void {
    if let Some(image) = crate::loader::image_containing(pc as usize) {
        if !base_out.is_null() {
            unsafe { base_out.write(image.base as *mut c_void) };
        }
        return image.base as *mut c_void;
    }
    std::ptr::null_mut()
}

pub(crate) unsafe extern "win64" fn terminate_process(_proc: *mut c_void, code: u32) -> i32 {
    eprintln!("[os-pe-loader] TerminateProcess({code}) dal modulo DAP.");
    std::process::exit(code as i32);
}

pub(crate) unsafe extern "win64" fn exit_process(code: u32) -> ! {
    std::process::exit(code as i32)
}

pub(crate) unsafe extern "win64" fn set_unhandled_exception_filter(_f: *mut c_void) -> *mut c_void {
    std::ptr::null_mut()
}

pub(crate) unsafe extern "win64" fn unhandled_exception_filter(_info: *mut c_void) -> i32 {
    0 // EXCEPTION_CONTINUE_SEARCH
}

// ---- vario CRT ----

pub(crate) unsafe extern "win64" fn output_debug_string_w(msg: *const u16) {
    if !VERBOSE.load(std::sync::atomic::Ordering::Relaxed) || msg.is_null() {
        return;
    }
    unsafe {
        let mut len = 0;
        while *msg.add(len) != 0 {
            len += 1;
        }
        eprintln!("[dap] {}", String::from_utf16_lossy(std::slice::from_raw_parts(msg, len)).trim_end());
    }
}

pub(crate) unsafe extern "win64" fn initialize_slist_head(slist: *mut c_void) {
    if !slist.is_null() {
        unsafe { std::ptr::write_bytes(slist as *mut u8, 0, 16) };
    }
}

pub(crate) unsafe extern "win64" fn interlocked_flush_slist(slist: *mut c_void) -> *mut c_void {
    if !slist.is_null() {
        unsafe { std::ptr::write_bytes(slist as *mut u8, 0, 16) };
    }
    std::ptr::null_mut()
}

/// STARTUPINFOW: 104 byte, il CRT legge la dimensione e gli handle.
pub(crate) unsafe extern "win64" fn get_startup_info_w(info: *mut u8) {
    if info.is_null() {
        return;
    }
    unsafe { std::ptr::write_bytes(info, 0, 104) };
    unsafe { *(info as *mut u32) = 104 };
}

pub(crate) unsafe extern "win64" fn decode_pointer(p: *mut c_void) -> *mut c_void {
    p
}

pub(crate) unsafe extern "win64" fn encode_pointer(p: *mut c_void) -> *mut c_void {
    p
}

/// CompareStringOrdinal: confronto per unità di codice, con opzione di ignorare il case
/// (i confronti del modulo sono nomi di DLL e classi WinRT, ASCII).
pub(crate) unsafe extern "win64" fn compare_string_ordinal(a: *const u16, alen: i32, b: *const u16, blen: i32, ignore_case: i32) -> i32 {
    unsafe {
        let read = |p: *const u16, n: i32| -> Vec<u16> {
            if p.is_null() {
                return Vec::new();
            }
            if n < 0 {
                let mut len = 0;
                while *p.add(len) != 0 {
                    len += 1;
                }
                std::slice::from_raw_parts(p, len).to_vec()
            } else {
                std::slice::from_raw_parts(p, n as usize).to_vec()
            }
        };
        let a = read(a, alen);
        let b = read(b, blen);
        let lower = |c: u16| if c < 0x80 { ((c as u8).to_ascii_lowercase()) as u16 } else { c };
        for i in 0..a.len().max(b.len()) {
            let (x, y) = (a.get(i).copied(), b.get(i).copied());
            let (x, y) = if ignore_case != 0 { (x.map(lower), y.map(lower)) } else { (x, y) };
            match (x, y) {
                (Some(x), Some(y)) if x != y => return if x < y { 1 } else { 3 },
                (Some(_), None) => return 3,
                (None, Some(_)) => return 1,
                _ => {}
            }
        }
    }
    2 // CSTR_EQUAL
}

// ---- tabelle codici e conversioni ----

pub(crate) unsafe extern "win64" fn is_valid_code_page(cp: u32) -> i32 {
    matches!(cp, 0 | 437 | 850 | 1252 | 65001 | 1200 | 1201).into()
}

pub(crate) unsafe extern "win64" fn get_acp() -> u32 {
    1252
}

pub(crate) unsafe extern "win64" fn get_oem_cp() -> u32 {
    437
}

pub(crate) unsafe extern "win64" fn get_thread_locale() -> u32 {
    1033 // en-US
}

/// GetCPInfo: 20 byte (MaxCharSize, DefaultChar[2], LeadByte[12]).
pub(crate) unsafe extern "win64" fn get_cp_info(cp: u32, info: *mut u8) -> i32 {
    if info.is_null() {
        return 0;
    }
    unsafe {
        std::ptr::write_bytes(info, 0, 20);
        match cp {
            65001 | 1200 | 1201 => {
                *(info as *mut u32) = 4;
            }
            0 | 1252 => {
                *(info as *mut u32) = 1;
                *info.add(4) = b'?';
            }
            437 | 850 => {
                *(info as *mut u32) = 1;
                *info.add(4) = b'?';
            }
            _ => return 0,
        }
    }
    TRUE
}

/// MultiByteToWideChar per CP_ACP (1252 → Latin-1) e CP_UTF8.
pub(crate) unsafe extern "win64" fn multi_byte_to_wide_char(cp: u32, _flags: u32, src: *const u8, srclen: i32, dst: *mut u16, dstlen: i32) -> i32 {
    unsafe {
        let bytes: &[u8] = if srclen < 0 {
            if src.is_null() {
                return 0;
            }
            CStr::from_ptr(src as *const i8).to_bytes()
        } else {
            std::slice::from_raw_parts(src, srclen as usize)
        };
        let wide: Vec<u16> = if cp == 65001 {
            String::from_utf8_lossy(bytes).encode_utf16().collect()
        } else {
            // 1252 e affini: byte → U+0000..U+00FF (Latin-1; le differenze 0x80-0x9F non
            // compaiono nei nomi che il modulo converte).
            bytes.iter().map(|&b| b as u16).collect()
        };
        if dst.is_null() || dstlen == 0 {
            return (wide.len() + usize::from(srclen < 0)) as i32;
        }
        if wide.len() > dstlen as usize {
            teb::set_last_error(122 /* ERROR_INSUFFICIENT_BUFFER */);
            return 0;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
        if srclen < 0 {
            *dst.add(wide.len()) = 0;
            return (wide.len() + 1) as i32;
        }
        wide.len() as i32
    }
}

pub(crate) unsafe extern "win64" fn wide_char_to_multi_byte(cp: u32, _flags: u32, src: *const u16, srclen: i32, dst: *mut u8, dstlen: i32, _def: *const u8, _used: *mut i32) -> i32 {
    unsafe {
        let wide: &[u16] = if srclen < 0 {
            if src.is_null() {
                return 0;
            }
            let mut len = 0;
            while *src.add(len) != 0 {
                len += 1;
            }
            std::slice::from_raw_parts(src, len + 1)
        } else {
            std::slice::from_raw_parts(src, srclen as usize)
        };
        let bytes: Vec<u8> = if cp == 65001 {
            String::from_utf16_lossy(if srclen < 0 { &wide[..wide.len() - 1] } else { wide })
                .into_bytes()
        } else {
            wide.iter().map(|&w| if w <= 0xFF { w as u8 } else { b'?' }).collect()
        };
        if dst.is_null() || dstlen == 0 {
            return (bytes.len() + usize::from(srclen < 0)) as i32;
        }
        if bytes.len() > dstlen as usize {
            teb::set_last_error(122);
            return 0;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        bytes.len() as i32
    }
}

/// GetStringTypeW con CT_CTYPE1 (1): classi di carattere per il locale "C".
pub(crate) unsafe extern "win64" fn get_string_type_w(info_type: u32, src: *const u16, count: i32, out: *mut u16) -> i32 {
    if out.is_null() {
        return 0;
    }
    unsafe {
        let n = if count < 0 {
            let mut len = 0;
            while *src.add(len) != 0 {
                len += 1;
            }
            len + 1
        } else {
            count as usize
        };
        for i in 0..n {
            let c = *src.add(i);
            let mut t = 0u16;
            if info_type & 1 != 0 {
                if (c as u8 as u16) == c && (c as u8).is_ascii_uppercase() {
                    t |= 0x0001; // C1_UPPER
                }
                if (c as u8 as u16) == c && (c as u8).is_ascii_lowercase() {
                    t |= 0x0002; // C1_LOWER
                }
                if (c as u8 as u16) == c && (c as u8).is_ascii_digit() {
                    t |= 0x0004; // C1_DIGIT
                }
                if c == 0x20 || (0x09..=0x0D).contains(&c) {
                    t |= 0x0008; // C1_SPACE
                }
                if (c as u8 as u16) == c && (c as u8).is_ascii_graphic() && !(c as u8).is_ascii_alphanumeric() {
                    t |= 0x0010; // C1_PUNCT
                }
                if (c as u32) < 0x20 || c == 0x7F {
                    t |= 0x0020; // C1_CNTRL
                }
                if (c as u8 as u16) == c && (c as u8).is_ascii_alphabetic() {
                    t |= 0x0100; // C1_ALPHA
                }
                if (c as u8 as u16) == c && (c as u8).is_ascii_hexdigit() {
                    t |= 0x0080; // C1_XDIGIT
                }
                if c == 0x20 || c == 0x09 {
                    t |= 0x0040; // C1_BLANK
                }
            }
            *out.add(i) = t;
        }
    }
    TRUE
}

/// LCMapStringW: lower/upper ASCII per unità di codice.
pub(crate) unsafe extern "win64" fn lc_map_string_w(_locale: u32, flags: u32, src: *const u16, srclen: i32, dst: *mut u16, dstlen: i32) -> i32 {
    const LCMAP_LOWERCASE: u32 = 0x100;
    const LCMAP_UPPERCASE: u32 = 0x200;
    unsafe {
        if src.is_null() {
            return 0;
        }
        let n = if srclen < 0 {
            let mut len = 0;
            while *src.add(len) != 0 {
                len += 1;
            }
            len + 1
        } else {
            srclen as usize
        };
        let src = std::slice::from_raw_parts(src, n);
        let map = |c: u16| -> u16 {
            if flags & LCMAP_LOWERCASE != 0 {
                if c < 0x80 { ((c as u8).to_ascii_lowercase()) as u16 } else { c }
            } else if flags & LCMAP_UPPERCASE != 0 {
                if c < 0x80 { ((c as u8).to_ascii_uppercase()) as u16 } else { c }
            } else {
                c
            }
        };
        let out: Vec<u16> = src.iter().map(|&c| map(c)).collect();
        if dst.is_null() || dstlen == 0 {
            return out.len() as i32;
        }
        if out.len() > dstlen as usize {
            teb::set_last_error(122);
            return 0;
        }
        std::ptr::copy_nonoverlapping(out.as_ptr(), dst, out.len());
        out.len() as i32
    }
}

/// Messaggi d'errore di sistema (FormatMessage FROM_SYSTEM + ALLOCATE_BUFFER).
fn win32_message(code: u32) -> String {
    match code {
        0 => "The operation completed successfully.".into(),
        2 => "The system cannot find the file specified.".into(),
        3 => "The system cannot find the path specified.".into(),
        5 => "Access is denied.".into(),
        6 => "The handle is invalid.".into(),
        8 => "Not enough memory resources are available to process this command.".into(),
        87 => "The parameter is incorrect.".into(),
        122 => "The data area passed to a system call is too small.".into(),
        126 => "The specified module could not be found.".into(),
        127 => "The specified procedure could not be found.".into(),
        1460 => "This operation returned because the timeout period expired.".into(),
        _ => format!("Unknown error {code}."),
    }
}

pub(crate) unsafe extern "win64" fn format_message_w(
    flags: u32,
    _source: *const c_void,
    _msgid: u32,
    _lang: u32,
    out: *mut *mut u16,
    _size: u32,
    _args: *const c_void,
) -> u32 {
    const FORMAT_MESSAGE_ALLOCATE_BUFFER: u32 = 0x100;
    const FORMAT_MESSAGE_FROM_SYSTEM: u32 = 0x1000;
    let msg = win32_message(teb::last_error());
    if flags & FORMAT_MESSAGE_FROM_SYSTEM == 0 {
        return 0;
    }
    let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
    if flags & FORMAT_MESSAGE_ALLOCATE_BUFFER != 0 {
        let p = crate::shims::heap::alloc_bytes(wide.len() * 2, false);
        if p.is_null() {
            return 0;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p as *mut u16, wide.len());
            if !out.is_null() {
                *out = p as *mut u16;
            }
        }
        (wide.len() - 1) as u32
    } else {
        0
    }
}

pub(crate) unsafe extern "win64" fn format_message_a(
    flags: u32,
    _source: *const c_void,
    _msgid: u32,
    _lang: u32,
    out: *mut i8,
    size: u32,
    _args: *const c_void,
) -> u32 {
    const FORMAT_MESSAGE_FROM_SYSTEM: u32 = 0x1000;
    const FORMAT_MESSAGE_ALLOCATE_BUFFER: u32 = 0x100;
    if flags & FORMAT_MESSAGE_FROM_SYSTEM == 0 {
        return 0;
    }
    let msg = win32_message(teb::last_error());
    if flags & FORMAT_MESSAGE_ALLOCATE_BUFFER != 0 {
        let p = crate::shims::heap::alloc_bytes(msg.len() + 1, false);
        if p.is_null() {
            return 0;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(msg.as_ptr(), p, msg.len());
            *p.add(msg.len()) = 0;
            *(out as *mut *mut i8) = p as *mut i8;
        }
        msg.len() as u32
    } else if !out.is_null() && size > 0 {
        let n = (size as usize - 1).min(msg.len());
        unsafe {
            std::ptr::copy_nonoverlapping(msg.as_ptr(), out as *mut u8, n);
            *out.add(n) = 0;
        }
        n as u32
    } else {
        0
    }
}

// ---- ambiente ----

pub(crate) unsafe extern "win64" fn get_environment_variable_a(_name: *const i8, _buf: *mut i8, _size: u32) -> u32 {
    0 // variabile non trovata
}

pub(crate) unsafe extern "win64" fn get_environment_strings_w() -> *mut u16 {
    // Blocco vuoto: solo il doppio NUL di terminazione.
    let block = crate::shims::heap::alloc_bytes(4, false);
    unsafe {
        if !block.is_null() {
            std::ptr::write_bytes(block, 0, 4);
        }
    }
    block as *mut u16
}

pub(crate) unsafe extern "win64" fn free_environment_strings_w(_block: *mut u16) -> i32 {
    unsafe { crate::shims::heap::free_bytes(_block as *mut u8) };
    TRUE
}

pub(crate) unsafe extern "win64" fn get_command_line_a() -> *mut i8 {
    static CMDLINE: &[u8] = b"os-dap-linux\0";
    CMDLINE.as_ptr() as *const i8 as *mut i8
}

pub(crate) unsafe extern "win64" fn get_command_line_w() -> *mut u16 {
    use std::sync::OnceLock;
    static CACHE: OnceLock<usize> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let wide: Vec<u16> = "os-dap-linux".encode_utf16().chain(Some(0)).collect();
        let p = unsafe { crate::shims::heap::alloc_bytes(wide.len() * 2, false) };
        unsafe { std::ptr::copy_nonoverlapping(wide.as_ptr(), p as *mut u16, wide.len()) };
        p as usize
    }) as *mut u16
}

pub(crate) unsafe extern "win64" fn get_std_handle(which: u32) -> *mut c_void {
    match which {
        0xFFFF_FFF6 /* STD_INPUT_HANDLE -10 */ => STD_INPUT as *mut c_void,
        0xFFFF_FFF5 /* STD_OUTPUT_HANDLE -11 */ => STD_OUTPUT as *mut c_void,
        0xFFFF_FFF4 /* STD_ERROR_HANDLE -12 */ => STD_ERROR as *mut c_void,
        _ => std::ptr::null_mut(),
    }
}

pub(crate) unsafe extern "win64" fn set_std_handle(_which: u32, _h: *mut c_void) -> i32 {
    TRUE
}

// ---- console e file (solo handle standard) ----

pub(crate) unsafe extern "win64" fn get_console_mode(_h: *mut c_void, _mode: *mut u32) -> i32 {
    teb::set_last_error(6);
    0 // come output rediretto: il CRT userà WriteFile
}

pub(crate) unsafe extern "win64" fn get_console_output_cp() -> u32 {
    65001
}

pub(crate) unsafe extern "win64" fn write_console_w(h: *mut c_void, buf: *const u16, chars: u32, written: *mut u32, _res: *const c_void) -> i32 {
    let fd = match h as u64 {
        h if h == STD_OUTPUT => 1,
        h if h == STD_ERROR => 2,
        _ => 1,
    };
    unsafe {
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(buf, chars as usize));
        let bytes = s.as_bytes();
        let n = libc::write(fd, bytes.as_ptr() as *const c_void, bytes.len());
        if n < 0 {
            return 0;
        }
        if !written.is_null() {
            written.write(chars);
        }
    }
    TRUE
}

pub(crate) unsafe extern "win64" fn create_file_w(
    _name: *const u16,
    _access: u32,
    _share: u32,
    _sa: *const c_void,
    _disp: u32,
    _flags: u32,
    _templ: *mut c_void,
) -> *mut c_void {
    // Nessun file di configurazione esiste: come Windows quando il percorso non c'è.
    teb::set_last_error(2);
    INVALID_HANDLE_VALUE as *mut c_void
}

pub(crate) unsafe extern "win64" fn find_first_file_ex_w(_name: *const u16, _info: u32, _data: *mut c_void, _search: u32, _filter: *const c_void, _flags: u32) -> *mut c_void {
    teb::set_last_error(3);
    INVALID_HANDLE_VALUE as *mut c_void
}

pub(crate) unsafe extern "win64" fn find_next_file_w(_h: *mut c_void, _data: *mut c_void) -> i32 {
    0
}

pub(crate) unsafe extern "win64" fn find_close(_h: *mut c_void) -> i32 {
    0
}

pub(crate) unsafe extern "win64" fn get_file_size_ex(h: *mut c_void, out: *mut i64) -> i32 {
    if (h as u64) == STD_INPUT || (h as u64) == STD_OUTPUT || (h as u64) == STD_ERROR {
        teb::set_last_error(6);
        return 0;
    }
    if !out.is_null() {
        unsafe { out.write(0) };
    }
    TRUE
}

pub(crate) unsafe extern "win64" fn get_file_type(h: *mut c_void) -> u32 {
    match h as u64 {
        h if h == STD_INPUT || h == STD_OUTPUT || h == STD_ERROR => 3, // FILE_TYPE_PIPE: il CRT userà WriteFile
        _ => 0,                                                       // FILE_TYPE_UNKNOWN
    }
}

pub(crate) unsafe extern "win64" fn flush_file_buffers(h: *mut c_void) -> i32 {
    match h as u64 {
        h if h == STD_OUTPUT || h == STD_ERROR => TRUE,
        _ => 0,
    }
}

pub(crate) unsafe extern "win64" fn write_file(h: *mut c_void, buf: *const c_void, size: u32, written: *mut u32, _overlapped: *mut c_void) -> i32 {
    let fd = match h as u64 {
        h if h == STD_OUTPUT => 1,
        h if h == STD_ERROR => 2,
        h if h == STD_INPUT => 0,
        _ => {
            teb::set_last_error(6);
            return 0;
        }
    };
    unsafe {
        let n = libc::write(fd, buf, size as usize);
        if n < 0 {
            teb::set_last_error(29);
            return 0;
        }
        if !written.is_null() {
            written.write(n as u32);
        }
    }
    TRUE
}

pub(crate) unsafe extern "win64" fn set_file_pointer_ex(_h: *mut c_void, _dist: i64, _new: *mut i64, _method: u32) -> i32 {
    teb::set_last_error(1);
    0
}

// ---- MessageBox: gli assert del CRT statico finiscono qui (via GetProcAddress) ----

const IDABORT: i32 = 3;

pub(crate) unsafe extern "win64" fn message_box_w(_hwnd: *mut c_void, text: *const u16, caption: *const u16, _type: u32) -> i32 {
    unsafe {
        let s = |p: *const u16| -> String {
            if p.is_null() {
                return String::new();
            }
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
        };
        eprintln!("[dap] {}: {}", s(caption), s(text).replace("\n\n", "\n"));
    }
    IDABORT
}

pub(crate) unsafe extern "win64" fn message_box_a(_hwnd: *mut c_void, text: *const i8, caption: *const i8, _type: u32) -> i32 {
    unsafe {
        let s = |p: *const i8| -> String {
            if p.is_null() {
                String::new()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        eprintln!("[dap] {}: {}", s(caption), s(text));
    }
    IDABORT
}

// ---- AVRT ----

pub(crate) unsafe extern "win64" fn av_set_mm_thread_characteristics_w(_task: *const u16, idx: *mut u32) -> *mut c_void {
    // "Pro Audio": nessuna priorità reale da impostare nell'elaborazione offline.
    if !idx.is_null() {
        unsafe { idx.write(1) };
    }
    0x1000_0000usize as *mut c_void
}

pub(crate) unsafe extern "win64" fn av_revert_mm_thread_characteristics(_h: *mut c_void) -> i32 {
    TRUE
}

// ---- RTWorkQ ----

/// IRtwqAsyncResult: IUnknown (slot 0-2) + GetObject, GetState, GetStateNoAddRef,
/// GetStatus, SetStatus (slot 3-7). Il callback è chiamato inline da PutWorkItem.
#[repr(C)]
struct AsyncResult {
    vtbl: &'static AsyncResultVtbl,
    app_object: *mut c_void,
    callback: *mut c_void,
    hr: i32,
}

pub(crate) unsafe extern "win64" fn ar_query_interface(this: *mut c_void, iid: *const u8, out: *mut *mut c_void) -> i32 {
    let _ = (this, iid);
    // Il modulo non fa QI sul risultato (verificato: nessun uso noto); rispondiamo noi stessi.
    unsafe { out.write(this) };
    S_OK
}

pub(crate) unsafe extern "win64" fn ar_add_ref(_this: *mut c_void) -> u32 {
    2
}

pub(crate) unsafe extern "win64" fn ar_release(_this: *mut c_void) -> u32 {
    1
}

pub(crate) unsafe extern "win64" fn ar_get_object(this: *mut c_void, out: *mut *mut c_void) -> i32 {
    let r = this as *mut AsyncResult;
    unsafe { out.write((*r).app_object) };
    S_OK
}

pub(crate) unsafe extern "win64" fn ar_get_state(this: *mut c_void, out: *mut *mut c_void) -> i32 {
    let _ = this;
    unsafe { out.write(std::ptr::null_mut()) };
    S_OK
}

pub(crate) unsafe extern "win64" fn ar_get_state_no_add_ref(this: *mut c_void) -> *mut c_void {
    let _ = this;
    std::ptr::null_mut()
}

pub(crate) unsafe extern "win64" fn ar_get_status(this: *mut c_void) -> i32 {
    unsafe { (*(this as *mut AsyncResult)).hr }
}

pub(crate) unsafe extern "win64" fn ar_set_status(this: *mut c_void, hr: i32) -> i32 {
    unsafe { (*(this as *mut AsyncResult)).hr = hr };
    S_OK
}

pub(crate) unsafe extern "win64" fn ar_get_error_code(this: *mut c_void) -> i32 {
    unsafe { (*(this as *mut AsyncResult)).hr }
}

pub(crate) unsafe extern "win64" fn ar_set_error_code(this: *mut c_void, hr: i32) -> i32 {
    unsafe { (*(this as *mut AsyncResult)).hr = hr };
    S_OK
}

pub(crate) unsafe extern "win64" fn ar_get_result(this: *mut c_void) -> *mut c_void {
    unsafe { (*(this as *mut AsyncResult)).app_object }
}

#[repr(C)]
struct AsyncResultVtbl {
    query_interface: unsafe extern "win64" fn(*mut c_void, *const u8, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "win64" fn(*mut c_void) -> u32,
    release: unsafe extern "win64" fn(*mut c_void) -> u32,
    get_object: unsafe extern "win64" fn(*mut c_void, *mut *mut c_void) -> i32,
    get_state: unsafe extern "win64" fn(*mut c_void, *mut *mut c_void) -> i32,
    get_state_no_add_ref: unsafe extern "win64" fn(*mut c_void) -> *mut c_void,
    get_status: unsafe extern "win64" fn(*mut c_void) -> i32,
    set_status: unsafe extern "win64" fn(*mut c_void, i32) -> i32,
    get_error_code: unsafe extern "win64" fn(*mut c_void) -> i32,
    set_error_code: unsafe extern "win64" fn(*mut c_void, i32) -> i32,
    get_result: unsafe extern "win64" fn(*mut c_void) -> *mut c_void,
}

static ASYNC_RESULT_VTABLE: AsyncResultVtbl = AsyncResultVtbl {
    query_interface: ar_query_interface,
    add_ref: ar_add_ref,
    release: ar_release,
    get_object: ar_get_object,
    get_state: ar_get_state,
    get_state_no_add_ref: ar_get_state_no_add_ref,
    get_status: ar_get_status,
    set_status: ar_set_status,
    get_error_code: ar_get_error_code,
    set_error_code: ar_set_error_code,
    get_result: ar_get_result,
};

pub(crate) unsafe extern "win64" fn rtwq_create_async_result(
    app_object: *mut c_void,
    callback: *mut c_void,
    hr: i32,
    out: *mut *mut c_void,
) -> i32 {
    if out.is_null() {
        return 0x8000_0003u32 as i32;
    }
    let r = Box::new(AsyncResult { vtbl: &ASYNC_RESULT_VTABLE, app_object, callback, hr });
    unsafe { *out = Box::into_raw(r) as *mut c_void };
    S_OK
}

/// Il work item viene eseguito subito sul thread chiamante: stesse conseguenze di un
/// PutWorkItem su una coda sincrona, ordine a parte (parametri uguali).
pub(crate) unsafe extern "win64" fn rtwq_put_work_item(_queue: u32, _priority: i32, result: *mut c_void) -> i32 {
    let r = result as *mut AsyncResult;
    if r.is_null() {
        return 0x8000_0003u32 as i32;
    }
    let callback = unsafe { (*r).callback };
    if !callback.is_null() {
        // IRtwqAsyncCallback::Invoke = slot 3 della vtable del callback.
        unsafe {
            let vtbl = *(callback as *mut *const usize);
            let invoke: unsafe extern "win64" fn(*mut c_void, *mut c_void) = std::mem::transmute(*vtbl.add(3));
            invoke(callback, result);
        }
    }
    S_OK
}

// ---- SDDL ----

/// Il modulo costruisce una DACL per la mappa DSO: qui non significa nulla, basta un
/// descrittore non nullo che CreateFileMappingW (nostro) ignora.
pub(crate) unsafe extern "win64" fn convert_sddl_w(
    _sddl: *const u16,
    _revision: u32,
    sd_out: *mut *mut c_void,
    _size_out: *mut u32,
) -> i32 {
    // SECURITY_DESCRIPTOR minimo: revisione 1, controllo 0, il resto zero (self-relative no).
    let sd = crate::shims::heap::alloc_bytes(8, false);
    if sd.is_null() {
        return 0;
    }
    unsafe {
        *sd = 1;
        if !sd_out.is_null() {
            *sd_out = sd as *mut c_void;
        }
    }
    TRUE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_string_ordinal_cases() {
        let a: Vec<u16> = "kernel32.dll".encode_utf16().collect();
        let b: Vec<u16> = "KERNEL32.DLL".encode_utf16().collect();
        assert_eq!(unsafe { compare_string_ordinal(a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32, 1) }, 2);
        assert_eq!(unsafe { compare_string_ordinal(a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32, 0) }, 3);
    }

    #[test]
    fn mb_wc_roundtrip() {
        let s = b"hello";
        let mut w = [0u16; 16];
        let n = unsafe { multi_byte_to_wide_char(65001, 0, s.as_ptr(), s.len() as i32, w.as_mut_ptr(), 16) };
        assert_eq!(n, 5);
        let mut out = [0u8; 16];
        let m = unsafe { wide_char_to_multi_byte(65001, 0, w.as_ptr(), n, out.as_mut_ptr(), 16, std::ptr::null(), std::ptr::null_mut()) };
        assert_eq!(&out[..m as usize], s);
    }
}
