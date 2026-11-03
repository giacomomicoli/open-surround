//! Memoria condivisa e protezione pagine: CreateFileMappingW/OpenFileMappingW con memfd
//! (i "Global\…" del DSO non hanno restrizioni di privilegio qui: nessuna riscrittura del
//! nome come doveva fare l'host Windows), MapViewOfFile/UnmapViewOfFile, VirtualProtect.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{LazyLock, Mutex};

use crate::shims::handles;
use crate::teb;

/// handle → (fd memfd, dimensione)
static MAPPINGS: LazyLock<Mutex<HashMap<u64, (i32, usize)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// viste aperte: indirizzo → lunghezza mappata (per UnmapViewOfFile).
static VIEWS: LazyLock<Mutex<HashMap<usize, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// nome → handle (per OpenFileMappingW).
static NAMES: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

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

pub(crate) unsafe extern "win64" fn create_file_mapping_w(
    file: *mut c_void,
    _attrs: *const c_void, // la DACL del modulo (LocalService/SYSTEM) qui non significa nulla
    _protect: u32,
    size_high: u32,
    size_low: u32,
    name: *const u16,
) -> *mut c_void {
    // Solo mapping anonimi (INVALID_HANDLE_VALUE): il DSO non mappa file reali.
    if (file as usize) != 0xFFFF_FFFF_FFFF_FFFF && !file.is_null() {
        teb::set_last_error(6);
        return std::ptr::null_mut();
    }
    let size = ((size_high as u64) << 32) | size_low as u64;
    if size == 0 {
        teb::set_last_error(87);
        return std::ptr::null_mut();
    }
    let name_s = unsafe { read_wide(name) };
    if !name_s.is_empty() {
        if let Some(&h) = NAMES.lock().unwrap().get(&name_s) {
            return h as *mut c_void;
        }
    }
    let fd = unsafe { libc::memfd_create(b"win-mapping\0".as_ptr() as *const i8, 0) };
    if fd < 0 {
        teb::set_last_error(8);
        return std::ptr::null_mut();
    }
    if unsafe { libc::ftruncate(fd, size as i64) } != 0 {
        unsafe { libc::close(fd) };
        teb::set_last_error(8);
        return std::ptr::null_mut();
    }
    let h = handles::next_handle();
    MAPPINGS.lock().unwrap().insert(h, (fd, size as usize));
    if !name_s.is_empty() {
        NAMES.lock().unwrap().insert(name_s, h);
    }
    h as *mut c_void
}

pub(crate) unsafe extern "win64" fn open_file_mapping_w(_access: u32, _inherit: i32, name: *const u16) -> *mut c_void {
    let name_s = unsafe { read_wide(name) };
    match NAMES.lock().unwrap().get(&name_s).copied() {
        Some(h) => h as *mut c_void,
        None => {
            teb::set_last_error(2 /* ERROR_FILE_NOT_FOUND */);
            std::ptr::null_mut()
        }
    }
}

pub(crate) unsafe extern "win64" fn map_view_of_file(
    h: *mut c_void,
    _access: u32,
    off_high: u32,
    off_low: u32,
    size: usize,
) -> *mut c_void {
    let Some((fd, mapping_size)) = MAPPINGS.lock().unwrap().get(&(h as u64)).copied() else {
        teb::set_last_error(6);
        return std::ptr::null_mut();
    };
    let off = (((off_high as u64) << 32) | off_low as u64) as usize;
    if off >= mapping_size {
        teb::set_last_error(87);
        return std::ptr::null_mut();
    }
    let len = if size == 0 { mapping_size - off } else { size };
    let p = unsafe {
        libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, off as i64)
    };
    if p == libc::MAP_FAILED {
        teb::set_last_error(8);
        return std::ptr::null_mut();
    }
    VIEWS.lock().unwrap().insert(p as usize, len);
    p as *mut c_void
}

pub(crate) unsafe extern "win64" fn unmap_view_of_file(p: *mut c_void) -> i32 {
    let len = VIEWS.lock().unwrap().remove(&(p as usize));
    match len {
        Some(len) => (unsafe { libc::munmap(p, len) == 0 }) as i32,
        None => {
            teb::set_last_error(87);
            0
        }
    }
}

/// PAGE_* → prot mmap.
unsafe fn prot_of(page: u32) -> i32 {
    let exec = page & 0xF0 != 0;
    let read = matches!(page & 0xFF, 2 | 4 | 8 | 0x20 | 0x40 | 0x80);
    let write = matches!(page & 0xFF, 4 | 8 | 0x40 | 0x80);
    let mut prot = 0;
    if read { prot |= libc::PROT_READ }
    if write { prot |= libc::PROT_WRITE }
    if exec { prot |= libc::PROT_EXEC }
    if prot == 0 { prot = libc::PROT_NONE }
    prot
}

pub(crate) unsafe extern "win64" fn virtual_protect(addr: *mut c_void, size: usize, new_protect: u32, old: *mut u32) -> i32 {
    if !old.is_null() {
        unsafe { old.write(4 /* PAGE_READWRITE, il più comune */) };
    }
    if addr.is_null() || size == 0 {
        return 1;
    }
    // Le pagine del modulo hanno già protezioni giuste; si applicano le nuove come chiesto.
    let page = (addr as usize).next_multiple_of(0x1000);
    let len = (size + (addr as usize - page) + 0xFFF) / 0x1000 * 0x1000;
    unsafe { libc::mprotect(page as *mut c_void, len, prot_of(new_protect)) };
    1
}
