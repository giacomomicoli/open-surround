//! Hook nella Import Address Table del modulo DAP.
//!
//! In SetAPOInitParameters il modulo crea la memoria condivisa DSO come
//! `Global\<sddl>MAPPING-FILE-<endpoint>`: in audiodg (LocalService) funziona, in un processo
//! utente fallisce per mancanza di SeCreateGlobalPrivilege, e il modulo allora non valida il
//! contesto APO (ConfigureEncoder risponde E_FAIL). Qui reindirizziamo il nome in `Local\`.

use std::ffi::{c_void, CStr};

use windows::Win32::Foundation::{HANDLE, HMODULE};
use windows::Win32::System::LibraryLoader::GetProcAddress;
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Diagnostics::Debug::{IMAGE_DIRECTORY_ENTRY_IMPORT, IMAGE_NT_HEADERS64};
use windows::Win32::System::Memory::{
    CreateFileMappingW, OpenFileMappingW, VirtualProtect, PAGE_PROTECTION_FLAGS, PAGE_READWRITE,
};
use windows::Win32::System::SystemServices::{
    IMAGE_DOS_HEADER, IMAGE_IMPORT_BY_NAME, IMAGE_IMPORT_DESCRIPTOR,
};
use windows::Win32::System::WindowsProgramming::IMAGE_THUNK_DATA64;
use windows_core::{BOOL, GUID, HSTRING, PCSTR, PCWSTR};

use crate::stubs::VERBOSE;

fn localize(name: PCWSTR) -> Option<HSTRING> {
    if name.is_null() {
        return None;
    }
    let s = unsafe { name.to_string() }.ok()?;
    let rest = s.strip_prefix("Global\\")?;
    if VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("[hook] {s} -> Local\\{rest}");
    }
    Some(HSTRING::from(format!("Local\\{rest}")))
}

unsafe extern "system" fn create_file_mapping_w(
    file: HANDLE,
    attrs: *const SECURITY_ATTRIBUTES,
    protect: PAGE_PROTECTION_FLAGS,
    size_high: u32,
    size_low: u32,
    name: PCWSTR,
) -> HANDLE {
    match localize(name) {
        // Il DACL del modulo concede accesso solo a LocalService e SYSTEM: lo scartiamo.
        Some(local) => CreateFileMappingW(file, None, protect, size_high, size_low, &local),
        None => CreateFileMappingW(file, attrs.as_ref().map(|a| a as *const _), protect, size_high, size_low, name),
    }
    .unwrap_or_default()
}

unsafe extern "system" fn open_file_mapping_w(access: u32, inherit: BOOL, name: PCWSTR) -> HANDLE {
    let local = localize(name);
    let name = local.as_ref().map(|h| PCWSTR(h.as_ptr())).unwrap_or(name);
    OpenFileMappingW(access, inherit.as_bool(), name).unwrap_or_default()
}

const IDABORT: i32 = 3;

/// Il CRT statico del modulo mostra gli assert con MessageBox (risolta via GetProcAddress):
/// li stampiamo su stderr e rispondiamo "Interrompi", così nessun dialogo blocca l'host.
unsafe extern "system" fn message_box_w(_hwnd: *mut c_void, text: PCWSTR, caption: PCWSTR, _type: u32) -> i32 {
    eprintln!(
        "[dap] {}: {}",
        caption.to_string().unwrap_or_default(),
        text.to_string().unwrap_or_default().replace("\n\n", "\n")
    );
    IDABORT
}

unsafe extern "system" fn message_box_a(_hwnd: *mut c_void, text: *const i8, caption: *const i8, _type: u32) -> i32 {
    let s = |p: *const i8| if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() };
    eprintln!("[dap] {}: {}", s(caption), s(text));
    IDABORT
}

unsafe extern "system" fn get_proc_address(hmod: HMODULE, name: PCSTR) -> *const c_void {
    // Valori < 0x10000 sono ordinali, non nomi.
    if name.0 as usize >= 0x10000 {
        match CStr::from_ptr(name.0 as *const i8).to_bytes() {
            b"MessageBoxW" => return message_box_w as *const c_void,
            b"MessageBoxA" => return message_box_a as *const c_void,
            _ => {}
        }
    }
    GetProcAddress(hmod, name).map_or(std::ptr::null(), |f| f as *const c_void)
}

/// Log del modulo: va su un provider ETW ({ebfc27e1-9503-4ad5-bf27-8c6bc695c59a}) che senza una
/// sessione ETW (serve admin) resta spento. Con -v lo abilitiamo noi al livello massimo e
/// stampiamo gli EventWriteString su stderr.
type EnableCallback = unsafe extern "system" fn(*const GUID, u32, u8, u64, u64, *const c_void, *mut c_void);
const FAKE_REG_HANDLE: u64 = 0x0e75_0000_0000_0001;

unsafe extern "system" fn event_register(
    provider: *const GUID,
    callback: Option<EnableCallback>,
    context: *mut c_void,
    handle: *mut u64,
) -> u32 {
    *handle = FAKE_REG_HANDLE;
    if let Some(cb) = callback {
        // EVENT_CONTROL_CODE_ENABLE_PROVIDER, livello 5 (verbose), tutte le keyword.
        cb(provider, 1, 5, u64::MAX, 0, std::ptr::null(), context);
    }
    0
}

unsafe extern "system" fn event_unregister(_handle: u64) -> u32 {
    0
}

unsafe extern "system" fn event_write_string(_handle: u64, level: u8, _keyword: u64, text: PCWSTR) -> u32 {
    eprintln!("[dap:{level}] {}", text.to_string().unwrap_or_default().trim_end());
    0
}

/// Sostituisce le voci IAT di `hmod` per CreateFileMappingW / OpenFileMappingW / GetProcAddress
/// e, se `etw_log`, per EventRegister / EventUnregister / EventWriteString.
/// Ritorna il numero di voci sostituite.
pub unsafe fn install(hmod: *mut c_void, etw_log: bool) -> usize {
    let base = hmod as *const u8;
    let dos = &*(base as *const IMAGE_DOS_HEADER);
    let nt = &*(base.offset(dos.e_lfanew as isize) as *const IMAGE_NT_HEADERS64);
    let dir = nt.OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_IMPORT.0 as usize];
    if dir.VirtualAddress == 0 {
        return 0;
    }
    let mut patched = 0;
    let mut desc = base.add(dir.VirtualAddress as usize) as *const IMAGE_IMPORT_DESCRIPTOR;
    while (*desc).Name != 0 {
        let names_rva = match (*desc).Anonymous.OriginalFirstThunk {
            0 => (*desc).FirstThunk,
            rva => rva,
        };
        let mut name_thunk = base.add(names_rva as usize) as *const IMAGE_THUNK_DATA64;
        let mut iat = base.add((*desc).FirstThunk as usize) as *mut IMAGE_THUNK_DATA64;
        while (*name_thunk).u1.AddressOfData != 0 {
            let data = (*name_thunk).u1.AddressOfData;
            if data & (1 << 63) == 0 {
                let by_name = base.add(data as usize) as *const IMAGE_IMPORT_BY_NAME;
                let fname = CStr::from_ptr((*by_name).Name.as_ptr() as *const i8);
                let target: Option<usize> = match fname.to_bytes() {
                    b"CreateFileMappingW" => Some(create_file_mapping_w as *const () as usize),
                    b"OpenFileMappingW" => Some(open_file_mapping_w as *const () as usize),
                    b"GetProcAddress" => Some(get_proc_address as *const () as usize),
                    b"EventRegister" if etw_log => Some(event_register as *const () as usize),
                    b"EventUnregister" if etw_log => Some(event_unregister as *const () as usize),
                    b"EventWriteString" if etw_log => Some(event_write_string as *const () as usize),
                    _ => None,
                };
                if let Some(t) = target {
                    let slot = &mut (*iat).u1.Function as *mut u64;
                    let mut old = PAGE_PROTECTION_FLAGS(0);
                    if VirtualProtect(slot as *const c_void, 8, PAGE_READWRITE, &mut old).is_ok() {
                        *slot = t as u64;
                        let _ = VirtualProtect(slot as *const c_void, 8, old, &mut old);
                        patched += 1;
                    }
                }
            }
            name_thunk = name_thunk.add(1);
            iat = iat.add(1);
        }
        desc = desc.add(1);
    }
    patched
}
