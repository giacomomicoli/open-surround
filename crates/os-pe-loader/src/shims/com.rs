//! COM mínimo: la piccola parte di COM/OLEAUT32/PROPSYS che il modulo DAP importa.

use std::ffi::c_void;

use crate::teb;

const S_OK: i32 = 0;
const S_FALSE: i32 = 1;
const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;
const VT_EMPTY: u16 = 0;
const VT_LPWSTR: u16 = 0x1F;
const VT_CLSID: u16 = 0x48; // 72
const VT_BLOB: u16 = 0x41; // 65

/// Byte in memoria di un GUID Windows a partire dalla notazione u128 di `windows_core`
/// (stessa di `os_dap_core::props::Guid`): Data1/Data2/Data3 little-endian, Data4 big-endian
/// (perché la notazione testuale XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX elenca i byte di
/// Data4 proprio in quell'ordine). L'intero `to_le_bytes` NON è il layout giusto.
pub const fn guid_bytes(v: u128) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[0] = (v >> 96) as u8;
    b[1] = (v >> 104) as u8;
    b[2] = (v >> 112) as u8;
    b[3] = (v >> 120) as u8;
    b[4] = (v >> 80) as u8;
    b[5] = (v >> 88) as u8;
    b[6] = (v >> 64) as u8;
    b[7] = (v >> 72) as u8;
    // Data4: big-endian dei 64 bit bassi (la notazione testuale elenca questi byte in ordine).
    b[8] = (v >> 56) as u8;
    b[9] = (v >> 48) as u8;
    b[10] = (v >> 40) as u8;
    b[11] = (v >> 32) as u8;
    b[12] = (v >> 24) as u8;
    b[13] = (v >> 16) as u8;
    b[14] = (v >> 8) as u8;
    b[15] = v as u8;
    b
}

/// Inverso di [`guid_bytes`]: byte in memoria Windows → u128 nella notazione di `windows_core`.
pub fn guid_from_bytes(b: &[u8; 16]) -> u128 {
    let d1 = u32::from_le_bytes(b[0..4].try_into().unwrap()) as u128;
    let d2 = u16::from_le_bytes(b[4..6].try_into().unwrap()) as u128;
    let d3 = u16::from_le_bytes(b[6..8].try_into().unwrap()) as u128;
    let d4 = u64::from_be_bytes(b[8..16].try_into().unwrap()) as u128;
    (d1 << 96) | (d2 << 80) | (d3 << 64) | d4
}

pub(crate) unsafe extern "win64" fn co_initialize_ex(_res: *mut c_void, _model: u32) -> i32 {
    if teb::com_mark_initialized() {
        S_FALSE
    } else {
        S_OK
    }
}

pub(crate) unsafe extern "win64" fn co_uninitialize() {}

/// GUID v4 da /dev/urandom (al modulo serve solo un identificatore qualunque).
pub(crate) unsafe extern "win64" fn co_create_guid(guid: *mut u8) -> i32 {
    if guid.is_null() {
        return 0x8000_0003u32 as i32;
    }
    let mut rnd = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if f.read_exact(&mut rnd).is_err() {
            return 0x8000_4005u32 as i32;
        }
    }
    rnd[6] = (rnd[6] & 0x0F) | 0x40; // versione 4
    rnd[8] = (rnd[8] & 0x3F) | 0x80; // variante
    unsafe { std::ptr::copy_nonoverlapping(rnd.as_ptr(), guid, 16) };
    S_OK
}

/// StringFromGUID2: "{00000000-0000-0000-0000-000000000000}" + NUL; ritorna i caratteri scritti.
pub(crate) unsafe extern "win64" fn string_from_guid2(guid: *const u8, dst: *mut u16, max: i32) -> i32 {
    if guid.is_null() || dst.is_null() || max < 39 {
        return 0;
    }
    let b = unsafe { std::slice::from_raw_parts(guid, 16) };
    let (d1, d2, d3) = (u32::from_le_bytes([b[0], b[1], b[2], b[3]]), u16::from_le_bytes([b[4], b[5]]), u16::from_le_bytes([b[6], b[7]]));
    let tail: Vec<u16> = b[8..16].iter().map(|&x| x as u16).collect();
    let s = format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        d1, d2, d3, tail[0], tail[1], tail[2], tail[3], tail[4], tail[5], tail[6], tail[7]
    );
    let wide: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
        *dst.add(wide.len()) = 0;
    }
    (wide.len() + 1) as i32
}

// ---- Free-threaded marshaler (il modulo lo aggrega per essere "agile") ----

/// IUnknown minimo: QI risponde solo IUnknown/IMarshal con E_NOINTERFACE per il resto
/// (nessun marshalling tra apartment avviene nel nostro host).
pub(crate) unsafe extern "win64" fn ftm_query_interface(this: *mut c_void, iid: *const u8, out: *mut *mut c_void) -> i32 {
    let _ = this;
    unsafe { out.write(std::ptr::null_mut()) };
    if iid.is_null() {
        return 0x8000_0003u32 as i32;
    }
    const IID_IUNKNOWN: [u8; 16] = guid_bytes(0x0000_0000_0000_0000_c000_0000_0000_0046);
    const IID_IMARSHAL: [u8; 16] = guid_bytes(0x0000_0003_0000_0000_c000_0000_0000_0046);
    if unsafe { std::slice::from_raw_parts(iid, 16) } == IID_IUNKNOWN || unsafe { std::slice::from_raw_parts(iid, 16) } == IID_IMARSHAL {
        unsafe { out.write(this) };
        return S_OK;
    }
    E_NOINTERFACE
}

pub(crate) unsafe extern "win64" fn ftm_add_ref(_this: *mut c_void) -> u32 {
    2
}

pub(crate) unsafe extern "win64" fn ftm_release(_this: *mut c_void) -> u32 {
    1 // mai liberato: vive quanto il processo
}

#[repr(C)]
struct FtmVtbl {
    query_interface: unsafe extern "win64" fn(*mut c_void, *const u8, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "win64" fn(*mut c_void) -> u32,
    release: unsafe extern "win64" fn(*mut c_void) -> u32,
}

static FTM_VTABLE: FtmVtbl = FtmVtbl {
    query_interface: ftm_query_interface,
    add_ref: ftm_add_ref,
    release: ftm_release,
};

pub(crate) unsafe extern "win64" fn co_create_free_threaded_marshaler(_outer: *mut c_void, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return 0x8000_0003u32 as i32;
    }
    let obj = Box::new(FtmObj { vtbl: &FTM_VTABLE });
    unsafe { *out = Box::into_raw(obj) as *mut c_void };
    S_OK
}

#[repr(C)]
struct FtmObj {
    vtbl: &'static FtmVtbl,
}

// ---- PROPVARIANT ----

pub(crate) unsafe extern "win64" fn prop_variant_clear(pv: *mut c_void) -> i32 {
    if pv.is_null() {
        return 0x8000_0003u32 as i32;
    }
    let p = pv as *mut u8;
    unsafe {
        let vt = *(p as *const u16);
        match vt {
            VT_BLOB => {
                let blob = p.add(0x10) as *mut *mut u8;
                if !(*blob).is_null() {
                    crate::shims::heap::free_bytes(*blob);
                    *blob = std::ptr::null_mut();
                }
            }
            VT_LPWSTR => {
                let w = p.add(8) as *mut *mut u16;
                if !(*w).is_null() {
                    crate::shims::heap::free_bytes(*w as *mut u8);
                    *w = std::ptr::null_mut();
                }
            }
            VT_CLSID => {
                let g = p.add(8) as *mut *mut u8;
                if !(*g).is_null() {
                    crate::shims::heap::free_bytes(*g);
                    *g = std::ptr::null_mut();
                }
            }
            _ => {}
        }
        std::ptr::write_bytes(p, 0, 0x18); // azzera il PROPVARIANT (24 byte)
    }
    S_OK
}

pub(crate) unsafe extern "win64" fn init_prop_variant_from_clsid(clsid: *const u8, pv: *mut c_void) -> i32 {
    if clsid.is_null() || pv.is_null() {
        return 0x8000_0003u32 as i32;
    }
    let copy = crate::shims::heap::alloc_bytes(16, false);
    if copy.is_null() {
        return 0x8007_000Eu32 as i32;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(clsid, copy, 16);
        std::ptr::write_bytes(pv as *mut u8, 0, 0x18);
        *(pv as *mut u16) = VT_CLSID;
        *((pv as *mut u8).add(8) as *mut *mut u8) = copy;
    }
    S_OK
}

// ---- OLEAUT32 (per ordinale) ----

/// #2 SysAllocString.
pub(crate) unsafe extern "win64" fn oleaut32_sys_alloc_string(src: *const u16) -> *mut c_void {
    if src.is_null() {
        return std::ptr::null_mut();
    }
    let len = unsafe {
        let mut n = 0;
        while *src.add(n) != 0 {
            n += 1;
        }
        n
    };
    let p = crate::shims::heap::alloc_bytes(4 + 2 * (len + 1), false);
    if p.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        *(p as *mut u32) = len as u32;
        std::ptr::copy_nonoverlapping(src, p.add(4) as *mut u16, len + 1);
    }
    (unsafe { p.add(4) }) as *mut c_void
}

/// #6 SysFreeString.
pub(crate) unsafe extern "win64" fn oleaut32_sys_free_string(bstr: *mut u16) {
    if !bstr.is_null() {
        unsafe { crate::shims::heap::free_bytes(bstr.sub(4) as *mut u8) };
    }
}

/// #7 SysStringLen.
pub(crate) unsafe extern "win64" fn oleaut32_sys_string_len(bstr: *const u16) -> u32 {
    if bstr.is_null() {
        return 0;
    }
    unsafe { *((bstr as *const u8).sub(4) as *const u32) }
}

/// #200 GetErrorInfo: nessuna infrastruttura di errori, S_FALSE con puntatore nullo.
pub(crate) unsafe extern "win64" fn oleaut32_get_error_info(_reserved: u32, out: *mut *mut c_void) -> i32 {
    if !out.is_null() {
        unsafe { out.write(std::ptr::null_mut()) };
    }
    S_FALSE
}

/// #201 SetErrorInfo: scarta.
pub(crate) unsafe extern "win64" fn oleaut32_set_error_info(_reserved: u32, _err: *mut c_void) -> i32 {
    S_OK
}

#[allow(dead_code)]
const _: () = {
    let _ = VT_EMPTY;
};
