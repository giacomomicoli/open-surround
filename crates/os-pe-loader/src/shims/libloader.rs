//! Loader di modelli: il modulo chiede handle di DLL di sistema (kernel32, user32, api-ms-…,
//! combase) e risolve funzioni per nome con GetProcAddress. Gli handle "fake" finiscono in un
//! registro; GetProcAddress risolve contro la stessa tabella degli shim usata per la IAT.
//! FindResourceW/LoadResource/LockResource/SizeofResource leggono l'albero .rsrc del modulo
//! vero, che il loader ha registrato per base.

use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::loader::{image_by_base, image_containing};
use crate::pe::ImportName;
use crate::shims;
use crate::teb;

/// Base degli handle fake (vedi handles::HANDLE_BASE per la stessa idea).
const FAKE_BASE: u64 = 0x7000_0000_1000;

static FAKES: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_FAKE: AtomicU32 = AtomicU32::new(1);

fn fake_handle_for(name: &str) -> u64 {
    let key = name.to_ascii_lowercase();
    let mut fakes = FAKES.lock().unwrap();
    if let Some(&h) = fakes.get(&key) {
        return h;
    }
    let h = FAKE_BASE + 8 * NEXT_FAKE.fetch_add(1, Ordering::SeqCst) as u64;
    fakes.insert(key, h);
    h
}

fn fake_name_of(h: u64) -> Option<String> {
    if h < FAKE_BASE {
        return None;
    }
    let fakes = FAKES.lock().unwrap();
    fakes.iter().find(|(_, &v)| v == h).map(|(k, _)| k.clone())
}

unsafe fn read_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe {
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

/// Il nome di una DLL, con o senza estensione, il caso in cui il modulo chiede handle per nome.
fn handle_by_name(name: &str) -> u64 {
    let lower = name.to_ascii_lowercase();
    let lower = lower.strip_suffix(".dll").unwrap_or(&lower);
    // "kernel32" e simili diventano handle fake; il modulo vero è reperibile per base.
    fake_handle_for(&format!("{lower}.dll"))
}

pub(crate) unsafe extern "win64" fn load_library_ex_w(name: *const u16, _file: *mut c_void, _flags: u32) -> u64 {
    let name = unsafe { read_wide(name) };
    if name.is_empty() {
        teb::set_last_error(126);
        return 0;
    }
    handle_by_name(&name)
}

pub(crate) unsafe extern "win64" fn load_library_w(name: *const u16) -> u64 {
    unsafe { load_library_ex_w(name, std::ptr::null_mut(), 0) }
}

pub(crate) unsafe extern "win64" fn load_library_ex_a(name: *const i8, _file: *mut c_void, _flags: u32) -> u64 {
    if name.is_null() {
        teb::set_last_error(126);
        return 0;
    }
    let name = unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() };
    handle_by_name(&name)
}

pub(crate) unsafe extern "win64" fn load_library_a(name: *const i8) -> u64 {
    unsafe { load_library_ex_a(name, std::ptr::null_mut(), 0) }
}

pub(crate) unsafe extern "win64" fn free_library(_hmod: *mut c_void) -> i32 {
    1 // il modulo resta caricato per la vita del processo
}

pub(crate) unsafe extern "win64" fn get_module_handle_w(name: *const u16) -> *mut c_void {
    if name.is_null() {
        // Modulo dell'eseguibile: il CRT a volte lo chiede per recuperare le sue opzioni.
        return fake_handle_for("os-dap-linux.exe") as *mut c_void;
    }
    let n = unsafe { read_wide(name) };
    handle_by_name(&n) as *mut c_void
}

/// GetModuleHandleExW: GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS = 0x4.
pub(crate) unsafe extern "win64" fn get_module_handle_ex_w(flags: u32, any: *const c_void, out: *mut *mut c_void) -> i32 {
    unsafe { module_handle_ex(flags, any as usize, out) }
}

pub(crate) unsafe extern "win64" fn get_module_handle_ex_a(flags: u32, any: *const c_void, out: *mut *mut c_void) -> i32 {
    unsafe { module_handle_ex(flags, any as usize, out) }
}

unsafe fn module_handle_ex(flags: u32, any: usize, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return 0;
    }
    if flags & 0x4 != 0 {
        if let Some(image) = image_containing(any) {
            unsafe { *out = image.base as *mut c_void };
            return 1;
        }
        // Anche gli indirizzi dei nostri shim sono "dentro" un modulo fittizio.
        if let Some(name) = fake_name_of(any as u64) {
            unsafe { *out = fake_handle_for(&name) as *mut c_void };
            return 1;
        }
        teb::set_last_error(126);
        return 0;
    }
    // Per nome: pointer a stringa ANSI o wide non distinguibile qui; il modulo usa
    // GetModuleHandleExW. Tratta `any` come nome wide solo se non sembra un handle.
    if any < 0x1_0000_0000 {
        let h = fake_handle_for("os-dap-linux.exe");
        unsafe { *out = h as *mut c_void };
        return 1;
    }
    teb::set_last_error(126);
    0
}

pub(crate) unsafe extern "win64" fn get_module_file_name_w(hmod: *mut c_void, buf: *mut u16, size: u32) -> u32 {
    if buf.is_null() || size == 0 {
        return 0;
    }
    let base = hmod as usize;
    let path = if base == 0 {
        "os-dap-linux.exe".to_string()
    } else if let Some(image) = image_by_base(base) {
        image.path.to_string_lossy().into_owned()
    } else if let Some(name) = fake_name_of(base as u64) {
        format!("C:\\Windows\\System32\\{name}")
    } else {
        teb::set_last_error(126);
        return 0;
    };
    let wide: Vec<u16> = path.encode_utf16().collect();
    let n = (size as usize).saturating_sub(1).min(wide.len());
    unsafe {
        std::ptr::copy_nonoverlapping(wide.as_ptr(), buf, n);
        *buf.add(n) = 0;
    }
    n as u32 + 1
}

pub(crate) unsafe extern "win64" fn get_proc_address(hmod: *mut c_void, name: *const i8) -> *mut c_void {
    if name.is_null() {
        teb::set_last_error(87);
        return std::ptr::null_mut();
    }
    let name_addr = name as usize;
    // Valore piccolo = ordinale.
    let import = if name_addr < 0x1_0000 {
        ImportName::Ordinal(name_addr as u32)
    } else {
        let s = unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() };
        ImportName::Name(s)
    };
    // DLL a cui l'handle si riferisce: il modulo vero usa il suo nome, i fake il loro.
    let base = hmod as usize;
    let dll = if let Some(image) = image_by_base(base) {
        image
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        fake_name_of(base as u64).unwrap_or_else(|| "kernel32.dll".into())
    };
    match shims::resolve(&dll, &import) {
        Some(addr) => addr as *mut c_void,
        None => {
            teb::set_last_error(127 /* ERROR_PROC_NOT_FOUND */);
            std::ptr::null_mut()
        }
    }
}

pub(crate) unsafe extern "win64" fn disable_thread_library_calls(_hmod: *mut c_void) -> i32 {
    1
}

// ---- Risorse del modulo ----

/// HRSRC/HGLOBAL: voce con i dati della risorsa (puntatore e dimensione), leak fino alla
/// fine del processo (il modulo tiene la risorsa del tuning per la vita del modulo).
#[repr(C)]
struct ResEntry {
    ptr: *const u8,
    size: u32,
}

pub(crate) unsafe extern "win64" fn find_resource_w(hmod: *mut c_void, name: *const u16, typ: *const u16) -> *mut c_void {
    let name_id = name as usize;
    let type_id = typ as usize;
    if name_id >= 0x1_0000 || type_id >= 0x1_0000 {
        return std::ptr::null_mut(); // ricerca per nome non usata dal modulo
    }
    let image = match image_by_base(hmod as usize) {
        Some(i) => i,
        None => {
            teb::set_last_error(126);
            return std::ptr::null_mut();
        }
    };
    match image.find_resource(type_id as u32, name_id as u32) {
        Some((ptr, size)) => Box::into_raw(Box::new(ResEntry { ptr, size: size as u32 })) as *mut c_void,
        None => {
            teb::set_last_error(1813 /* ERROR_RESOURCE_TYPE_NOT_FOUND */);
            std::ptr::null_mut()
        }
    }
}

pub(crate) unsafe extern "win64" fn load_resource(_hmod: *mut c_void, hrsrc: *mut c_void) -> *mut c_void {
    hrsrc
}

pub(crate) unsafe extern "win64" fn lock_resource(hglobal: *mut c_void) -> *mut c_void {
    let entry = hglobal as *mut ResEntry;
    if entry.is_null() {
        return std::ptr::null_mut();
    }
    (unsafe { (*entry).ptr }) as *mut c_void
}

pub(crate) unsafe extern "win64" fn size_of_resource(_hmod: *mut c_void, hrsrc: *mut c_void) -> u32 {
    let entry = hrsrc as *mut ResEntry;
    if entry.is_null() {
        return 0;
    }
    unsafe { (*entry).size }
}
