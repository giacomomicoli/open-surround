//! Memoria condivisa e protezione pagine: CreateFileMappingW/OpenFileMappingW con memfd
//! (i "Global\…" del DSO non hanno restrizioni di privilegio qui: nessuna riscrittura del
//! nome come doveva fare l'host Windows), MapViewOfFile/UnmapViewOfFile, VirtualProtect.
//!
//! I file mapping sono oggetti kernel come gli altri (`handles`): stesso spazio dei nomi,
//! handle distinti per Create/Open, `ERROR_ALREADY_EXISTS`, chiusura con CloseHandle.

use std::collections::{BTreeMap, HashMap};
use std::ffi::c_void;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{LazyLock, Mutex};

use crate::shims::handles::{self, Kind, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NOT_ENOUGH_MEMORY};
use crate::teb;

const ERROR_NOACCESS: u32 = 998;
const ERROR_INVALID_ADDRESS: u32 = 487;
const PAGE: usize = 0x1000;

/// viste aperte: indirizzo → lunghezza mappata (per UnmapViewOfFile).
static VIEWS: LazyLock<Mutex<HashMap<usize, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

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
        teb::set_last_error(ERROR_INVALID_HANDLE);
        return std::ptr::null_mut();
    }
    let size = ((size_high as u64) << 32) | size_low as u64;
    if size == 0 {
        teb::set_last_error(ERROR_INVALID_PARAMETER);
        return std::ptr::null_mut();
    }
    let h = handles::create_object(&handles::wide_string(name), handles::is_mapping, || {
        let fd = unsafe { libc::memfd_create(c"win-mapping".as_ptr(), 0) };
        if fd < 0 {
            return Err(ERROR_NOT_ENOUGH_MEMORY);
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::ftruncate(fd.as_raw_fd(), size as i64) } != 0 {
            return Err(ERROR_NOT_ENOUGH_MEMORY);
        }
        Ok(Kind::Mapping { fd, size: size as usize })
    });
    h as *mut c_void
}

pub(crate) unsafe extern "win64" fn open_file_mapping_w(_access: u32, _inherit: i32, name: *const u16) -> *mut c_void {
    handles::open_object(&handles::wide_string(name), handles::is_mapping) as *mut c_void
}

pub(crate) unsafe extern "win64" fn map_view_of_file(
    h: *mut c_void,
    _access: u32,
    off_high: u32,
    off_low: u32,
    size: usize,
) -> *mut c_void {
    let obj = handles::get(h as u64);
    let Some(Kind::Mapping { fd, size: mapping_size }) = obj.as_deref().map(|o| &o.kind) else {
        teb::set_last_error(ERROR_INVALID_HANDLE);
        return std::ptr::null_mut();
    };
    let off = (((off_high as u64) << 32) | off_low as u64) as usize;
    if off >= *mapping_size || size > *mapping_size - off {
        teb::set_last_error(ERROR_INVALID_PARAMETER);
        return std::ptr::null_mut();
    }
    let len = if size == 0 { mapping_size - off } else { size };
    let p = unsafe {
        libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd.as_raw_fd(), off as i64)
    };
    if p == libc::MAP_FAILED {
        teb::set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return std::ptr::null_mut();
    }
    VIEWS.lock().unwrap().insert(p as usize, len);
    p
}

pub(crate) unsafe extern "win64" fn unmap_view_of_file(p: *mut c_void) -> i32 {
    let len = VIEWS.lock().unwrap().remove(&(p as usize));
    match len {
        Some(len) => (unsafe { libc::munmap(p, len) == 0 }) as i32,
        None => {
            teb::set_last_error(ERROR_INVALID_PARAMETER);
            0
        }
    }
}

// ---- protezione delle pagine ----

/// Protezione PAGE_* per pagina, come l'ha impostata il loader (sezioni) o VirtualProtect:
/// è ciò che VirtualProtect restituisce come protezione precedente. Le pagine che non sono
/// qui (heap, stack, mapping) si leggono da /proc/self/maps.
static PROTECTIONS: LazyLock<Mutex<BTreeMap<usize, u32>>> = LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// PAGE_* (byte basso; PAGE_GUARD, PAGE_NOCACHE e simili non hanno equivalente) → prot mmap.
/// None per un valore che non è una sola protezione di base.
pub(crate) fn prot_of(page: u32) -> i32 {
    prot_of_checked(page).unwrap_or(libc::PROT_NONE)
}

fn prot_of_checked(page: u32) -> Option<i32> {
    use libc::{PROT_EXEC as X, PROT_NONE, PROT_READ as R, PROT_WRITE as W};
    Some(match page & 0xFF {
        0x01 => PROT_NONE, // PAGE_NOACCESS
        0x02 => R,         // PAGE_READONLY
        0x04 | 0x08 => R | W, // PAGE_READWRITE, PAGE_WRITECOPY
        0x10 => X,         // PAGE_EXECUTE
        0x20 => R | X,     // PAGE_EXECUTE_READ
        0x40 | 0x80 => R | W | X, // PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY
        _ => return None,
    })
}

/// Il loader registra le protezioni delle sezioni dopo averle applicate.
pub(crate) fn record_protection(addr: usize, len: usize, page: u32) {
    let mut map = PROTECTIONS.lock().unwrap();
    for p in (addr & !(PAGE - 1)..addr + len).step_by(PAGE) {
        map.insert(p, page);
    }
}

/// Protezione attuale di una pagina non registrata, da /proc/self/maps (None = non mappata).
fn protection_from_maps(page: usize) -> Option<u32> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    for line in maps.lines() {
        let (range, rest) = line.split_once(' ')?;
        let (lo, hi) = range.split_once('-')?;
        let (lo, hi) = (usize::from_str_radix(lo, 16).ok()?, usize::from_str_radix(hi, 16).ok()?);
        if page >= lo && page < hi {
            let perms = rest.as_bytes();
            return Some(match (perms[0] == b'r', perms[1] == b'w', perms[2] == b'x') {
                (_, true, true) => 0x40,
                (true, false, true) => 0x20,
                (false, false, true) => 0x10,
                (_, true, false) => 0x04,
                (true, false, false) => 0x02,
                (false, false, false) => 0x01,
            });
        }
    }
    None
}

/// `BOOL VirtualProtect(void* addr, SIZE_T size, DWORD newProtect, DWORD* oldProtect)`: le
/// pagine toccate sono tutte quelle che contengono almeno un byte di `[addr, addr+size)`;
/// `*oldProtect` riceve la protezione della prima.
pub(crate) unsafe extern "win64" fn virtual_protect(addr: *mut c_void, size: usize, new_protect: u32, old: *mut u32) -> i32 {
    if old.is_null() {
        teb::set_last_error(ERROR_NOACCESS);
        return 0;
    }
    let Some(prot) = prot_of_checked(new_protect) else {
        teb::set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    };
    let addr = addr as usize;
    let start = addr & !(PAGE - 1);
    let Some(end) = addr.checked_add(size).and_then(|e| e.checked_next_multiple_of(PAGE)) else {
        teb::set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    };
    let end = end.max(start + PAGE); // size 0: la pagina di `addr`
    let recorded = PROTECTIONS.lock().unwrap().get(&start).copied();
    let Some(previous) = recorded.or_else(|| protection_from_maps(start)) else {
        teb::set_last_error(ERROR_INVALID_ADDRESS);
        return 0;
    };
    if unsafe { libc::mprotect(start as *mut c_void, end - start, prot) } != 0 {
        teb::set_last_error(ERROR_INVALID_ADDRESS);
        return 0;
    }
    record_protection(start, end - start, new_protect & 0xFF);
    unsafe { old.write(previous) };
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page_rw() -> usize {
        let p = unsafe {
            libc::mmap(std::ptr::null_mut(), 2 * PAGE, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0)
        };
        assert_ne!(p, libc::MAP_FAILED);
        p as usize
    }

    #[test]
    fn unaligned_address_and_real_old_protection() {
        teb::install();
        let base = page_rw();
        let mut old = 0u32;
        unsafe {
            // indirizzo non allineato dentro una pagina nostra (prima: overflow → panic)
            assert_eq!(virtual_protect((base + 0x123) as *mut c_void, 0x10, 0x02, &mut old), 1);
            assert_eq!(old, 0x04, "RW da /proc/self/maps");
            assert_eq!(virtual_protect((base + 0x800) as *mut c_void, 0x10, 0x04, &mut old), 1);
            assert_eq!(old, 0x02, "RO → RW restituisce RO");
            // a cavallo di due pagine: entrambe cambiano
            assert_eq!(virtual_protect((base + 0xFF0) as *mut c_void, 0x20, 0x02, &mut old), 1);
            assert_eq!(protection_from_maps(base + PAGE), Some(0x02));
            assert_eq!(virtual_protect(base as *mut c_void, 2 * PAGE, 0x04, &mut old), 1);
            assert_eq!(old, 0x02);
            *((base + 0x10) as *mut u8) = 1; // di nuovo scrivibile
            assert_eq!(virtual_protect(base as *mut c_void, 1, 0x04, std::ptr::null_mut()), 0);
            assert_eq!(teb::last_error(), ERROR_NOACCESS);
            assert_eq!(virtual_protect(base as *mut c_void, 1, 0x1234, &mut old), 0);
            assert_eq!(teb::last_error(), ERROR_INVALID_PARAMETER);
            // Prima i record, poi il munmap: dopo, un altro test può ricevere lo stesso indirizzo.
            PROTECTIONS.lock().unwrap().retain(|&p, _| p < base || p >= base + 2 * PAGE);
            libc::munmap(base as *mut c_void, 2 * PAGE);
        }
    }

    /// Pagina non mappata. Si usa una pagina sotto `vm.mmap_min_addr` (64 KiB di default), che
    /// nessun mmap del processo può ottenere: prima il test la provava sull'indirizzo appena
    /// liberato con munmap, ma i test girano in parallelo e un altro thread poteva mapparci
    /// sopra fra il munmap e la chiamata (2 fallimenti su 8 esecuzioni, doc 12.15).
    #[test]
    fn unmapped_page() {
        teb::install();
        const UNMAPPED: usize = 0x1000;
        assert_eq!(protection_from_maps(UNMAPPED), None, "pagina 0x1000 mappata: vm.mmap_min_addr è 0?");
        let mut old = 0u32;
        unsafe {
            assert_eq!(virtual_protect(UNMAPPED as *mut c_void, 1, 0x04, &mut old), 0);
            assert_eq!(teb::last_error(), ERROR_INVALID_ADDRESS);
            // Anche con una protezione registrata (come quella di una vista chiusa) mprotect
            // fallisce e l'errore è lo stesso.
            record_protection(UNMAPPED, PAGE, 0x04);
            let r = virtual_protect(UNMAPPED as *mut c_void, 1, 0x02, &mut old);
            let e = teb::last_error();
            PROTECTIONS.lock().unwrap().remove(&UNMAPPED);
            assert_eq!((r, e), (0, ERROR_INVALID_ADDRESS));
        }
    }

    #[test]
    fn named_mapping_shared_and_closed() {
        teb::install();
        let name: Vec<u16> = "Global\\MAPPING-FILE-test".encode_utf16().chain(Some(0)).collect();
        let inv = usize::MAX as *mut c_void;
        unsafe {
            let a = create_file_mapping_w(inv, std::ptr::null(), 4, 0, 0x1000, name.as_ptr());
            assert!(!a.is_null());
            assert_eq!(teb::last_error(), 0);
            let b = create_file_mapping_w(inv, std::ptr::null(), 4, 0, 0x1000, name.as_ptr());
            assert_eq!(teb::last_error(), 183);
            assert_ne!(a, b);
            let va = map_view_of_file(a, 0, 0, 0, 0) as *mut u32;
            let vb = map_view_of_file(b, 0, 0, 0, 0) as *mut u32;
            *va = 0xC0FFEE;
            assert_eq!(*vb, 0xC0FFEE, "stessa memoria");
            assert_eq!(handles::close_handle(a as u64), 1, "CloseHandle conosce i mapping");
            assert_eq!(handles::close_handle(b as u64), 1);
            assert_eq!(*va, 0xC0FFEE, "le viste sopravvivono all'handle");
            assert_eq!(unmap_view_of_file(va as *mut c_void), 1);
            assert_eq!(unmap_view_of_file(vb as *mut c_void), 1);
            assert!(open_file_mapping_w(0, 0, name.as_ptr()).is_null());
            assert_eq!(teb::last_error(), handles::ERROR_FILE_NOT_FOUND);
        }
    }
}
