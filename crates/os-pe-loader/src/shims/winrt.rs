//! HSTRING. L'intera famiglia di funzioni WinRT che il modulo importa passa da qui, quindi il
//! layout può essere nostro, purché coerente: il modulo crea riferimenti e stringhe nostre e
//! le rilegge con le nostre funzioni. Le stringhe "owned" portano la dimensione prima dei dati
//! (mai un magico basso: il primo u32 è il rifcount, che parte da 0x50000); i riferimenti
//! rapidi usano il buffer del chiamante con un magico "REFW".

use std::ffi::c_void;

const REF_MAGIC: u32 = u32::from_le_bytes(*b"REFW");
const REFCOUNT_BASE: u32 = 0x50000;
const EMPTY: [u16; 1] = [0];

/// Layout del buffer del chiamante per WindowsCreateStringReference (24 byte).
#[repr(C)]
struct HsRef {
    magic: u32,
    len: u32,
    ptr: *const u16,
}

/// Stringa "owned": [refcnt u32][len u32][dati u16...].
#[repr(C)]
struct HsOwned {
    refcnt: u32,
    len: u32,
}

/// Puntatore nullo o vuoto → buffer statico di una NUL.
unsafe fn buffer_of(hs: u64, len_out: *mut u32) -> (*const u16, u32) {
    if hs == 0 {
        if !len_out.is_null() {
            unsafe { len_out.write(0) };
        }
        return (EMPTY.as_ptr(), 0);
    }
    let p = hs as *const HsRef;
    unsafe {
        if (*p).magic == REF_MAGIC {
            let len = (*p).len;
            if !len_out.is_null() {
                len_out.write(len);
            }
            ((*p).ptr, len)
        } else {
            let o = hs as *const HsOwned;
            let len = (*o).len;
            if !len_out.is_null() {
                len_out.write(len);
            }
            ((hs as *const u8).add(8) as *const u16, len)
        }
    }
}

pub(crate) unsafe extern "win64" fn windows_create_string_reference(
    src: *const u16,
    len: u32,
    header: *mut c_void,
    hs_out: *mut u64,
) -> i32 {
    if header.is_null() || hs_out.is_null() {
        return 0x8000_0003u32 as i32; // E_INVALIDARG
    }
    unsafe {
        let h = header as *mut HsRef;
        if len == 0 {
            (*h).magic = REF_MAGIC;
            (*h).len = 0;
            (*h).ptr = EMPTY.as_ptr();
        } else {
            (*h).magic = REF_MAGIC;
            (*h).len = len;
            (*h).ptr = src;
        }
        hs_out.write(header as u64);
    }
    0
}

pub(crate) unsafe extern "win64" fn windows_create_string(src: *const u16, len: u32, hs_out: *mut u64) -> i32 {
    if hs_out.is_null() {
        return 0x8000_0003u32 as i32;
    }
    if src.is_null() || len == 0 {
        unsafe { hs_out.write(0) };
        return 0;
    }
    let bytes = 8 + 2 * (len as usize + 1);
    let p = crate::shims::heap::alloc_bytes(bytes, false);
    if p.is_null() {
        return 0x8007_000Eu32 as i32; // E_OUTOFMEMORY
    }
    unsafe {
        let o = p as *mut HsOwned;
        (*o).refcnt = REFCOUNT_BASE;
        (*o).len = len;
        std::ptr::copy_nonoverlapping(src, p.add(8) as *mut u16, len as usize);
        *(p.add(8) as *mut u16).add(len as usize) = 0;
        hs_out.write(p as u64);
    }
    0
}

pub(crate) unsafe extern "win64" fn windows_get_string_raw_buffer(hs: u64, len_out: *mut u32) -> *const u16 {
    unsafe { buffer_of(hs, len_out).0 }
}

pub(crate) unsafe extern "win64" fn windows_is_string_empty(hs: u64) -> i32 {
    let (_, len) = unsafe { buffer_of(hs, std::ptr::null_mut()) };
    (len == 0) as i32
}

pub(crate) unsafe extern "win64" fn windows_string_has_embedded_null(hs: u64) -> i32 {
    let (buf, len) = unsafe { buffer_of(hs, std::ptr::null_mut()) };
    unsafe { (0..len as usize).any(|i| *buf.add(i) == 0) as i32 }
}

pub(crate) unsafe extern "win64" fn ro_originate_error(_error: i32, _message: u64) -> i32 {
    0 // S_OK: nessuna infrastruttura di errori WinRT da alimentare
}

pub(crate) unsafe extern "win64" fn ro_originate_error_w(_error: i32, _max: u32, _message: *const u16) -> i32 {
    0
}

/// Riferimento rapido a una stringa UTF-16 che possiede i propri dati (i riferimenti Windows
/// puntano nel buffer del chiamante: qui il buffer vive nella struct, e l'HSTRING resta
/// valido finché la struct vive).
pub struct HStringRef {
    /// Dati UTF-16 con NUL finale: la memoria che l'HSTRING riferisce.
    wide: Box<[u16]>,
    /// Header HSTRING (24 byte): l'HSTRING è il puntatore a questo buffer.
    header: Box<[u8; 24]>,
}

impl HStringRef {
    pub fn new(s: &str) -> Self {
        let wide: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
        let mut header = Box::new([0u8; 24]);
        let mut hs: u64 = 0;
        let len = wide.len() - 1;
        // Il riferimento non include la NUL finale (convenzione WinRT).
        unsafe {
            windows_create_string_reference(
                wide.as_ptr(),
                len as u32,
                header.as_mut_ptr() as *mut c_void,
                &mut hs as *mut u64,
            );
        }
        debug_assert_eq!(hs, header.as_ptr() as u64);
        HStringRef { wide: wide.into(), header }
    }

    /// Il valore HSTRING da passare al modulo.
    pub fn handle(&self) -> u64 {
        self.header.as_ptr() as u64
    }

    #[allow(dead_code)]
    pub fn as_str(&self) -> String {
        String::from_utf16_lossy(&self.wide[..self.wide.len() - 1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hstring_reference_roundtrip() {
        let r = HStringRef::new("Dolby.DolbyAudioProcessingModule");
        let hs = r.handle();
        let mut len = 0u32;
        let buf = unsafe { windows_get_string_raw_buffer(hs, &mut len as *mut u32) };
        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(buf, len as usize) });
        assert_eq!(s, "Dolby.DolbyAudioProcessingModule");
        assert_eq!(unsafe { windows_is_string_empty(hs) }, 0);
        assert_eq!(unsafe { windows_string_has_embedded_null(hs) }, 0);
        drop(r); // i dati muoiono con il riferimento, come su Windows
    }

    #[test]
    fn hstring_owned_roundtrip() {
        let wide: Vec<u16> = "città".encode_utf16().collect();
        let mut hs = 0u64;
        unsafe { windows_create_string(wide.as_ptr(), wide.len() as u32, &mut hs as *mut u64) };
        let mut len = 0u32;
        let buf = unsafe { windows_get_string_raw_buffer(hs, &mut len as *mut u32) };
        assert_eq!(len as usize, wide.len());
        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(buf, len as usize) });
        assert_eq!(s, "città");
    }

    #[test]
    fn empty_string() {
        assert_eq!(unsafe { windows_is_string_empty(0) }, 1);
        let mut len = 5u32;
        let buf = unsafe { windows_get_string_raw_buffer(0, &mut len as *mut u32) };
        assert_eq!(len, 0);
        assert_eq!(unsafe { *buf }, 0);
    }
}
