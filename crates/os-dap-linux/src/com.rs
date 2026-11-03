//! Oggetti COM che il modulo DAP si aspetta da Windows (doc sez. 12.7), l'equivalente di
//! `os-dap-host/src/stubs.rs` ma con vtable scritte a mano: su Linux non c'è la crate
//! `windows` a fare da ponte, e il modulo chiama le vtable con ABI win64.
//!
//! IMMDeviceCollection → IMMDevice → IAudioSystemEffectsPropertyStore → IPropertyStore,
//! più l'IPropertyStore dell'endpoint. Tutto in memoria, nessuna MMDevAPI.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use os_dap_core::{ModuleSetup, PropKey, Value};

pub static VERBOSE: AtomicBool = AtomicBool::new(false);

macro_rules! trace {
    ($($arg:tt)*) => {
        if VERBOSE.load(Ordering::Relaxed) {
            eprintln!("[stub] {}", format!($($arg)*));
        }
    };
}

const S_OK: i32 = 0;
const E_POINTER: i32 = 0x8000_4003u32 as i32;
const E_NOTIMPL: i32 = 0x8000_4001u32 as i32;
const VT_BLOB: u16 = 0x41;
const VT_LPWSTR: u16 = 0x1F;

/// Byte in memoria di un GUID Windows (Data1/2/3 LE, Data4 BE) e inverso.
fn iid_bytes(u: u128) -> [u8; 16] {
    os_pe_loader::shims::com::guid_bytes(u)
}

fn iid_from_bytes(b: &[u8; 16]) -> u128 {
    os_pe_loader::shims::com::guid_from_bytes(b)
}

// Valori dell'SDK (windows-0.62.2: Media/Audio/mod.rs per IMMDevice e IMMDeviceCollection,
// UI/Shell/PropertiesSystem per IPropertyStore, Media/Audio/Apo per l'FX store): i test
// li confrontano con le stringhe degli header (mmdeviceapi.h, propsys.h, audioenginebaseapo.h).
const IID_IUNKNOWN: u128 = 0x0000_0000_0000_0000_c000_0000_0000_0046;
const IID_IPROPERTYSTORE: u128 = 0x886d_8eeb_8cf2_4446_8d02_cdba_1dbd_cf99;
const IID_IAUDIOSYSTEMEFFECTSPROPERTYSTORE: u128 = 0x302a_e7f9_d7e0_43e4_971b_1f82_9361_3d2a;
const IID_IMMDEVICE: u128 = 0xd666_063f_1587_4e43_81f1_b948_e807_363f;
const IID_IMMDEVICECOLLECTION: u128 = 0x0bd7_a1be_7a1a_44db_8397_cc53_9238_7b5e;

/// Allocazione coerente con CoTaskMemAlloc/CoTaskMemFree del modulo (stesso heap).
fn co_alloc(bytes: &[u8]) -> *mut u8 {
    let p = os_pe_loader::shims::heap::allocate(bytes.len().max(1), false);
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len()) };
    p
}

fn co_wstr(s: &str) -> *mut u16 {
    let w: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2) };
    co_alloc(bytes) as *mut u16
}

/// Costruisce un PROPVARIANT (24 byte) di proprietà: VT_BLOB o VT_LPWSTR.
/// Il modulo lo libera con PropVariantClear (che usa il nostro stesso heap).
unsafe fn write_propvariant(pv: *mut u8, v: &Value) {
    unsafe {
        std::ptr::write_bytes(pv, 0, 0x18);
        match v {
            Value::Blob(b) => {
                *(pv as *mut u16) = VT_BLOB;
                *(pv.add(8) as *mut u32) = b.len() as u32;
                *(pv.add(0x10) as *mut *mut u8) = co_alloc(b);
            }
            Value::Str(s) => {
                *(pv as *mut u16) = VT_LPWSTR;
                *(pv.add(8) as *mut *mut u16) = co_wstr(s);
            }
        }
    }
}

fn key_str(fmtid: u128, pid: u32) -> String {
    format!("{{{:?}}},{}", os_dap_core::Guid(fmtid), pid)
}

// ---- generico: IUnknown delle nostre vtable ----

unsafe extern "win64" fn unknown_query_interface(this: *mut c_void, iid: *const u8, out: *mut *mut c_void) -> i32 {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    unsafe { *out = std::ptr::null_mut() };
    let iid = unsafe { std::slice::from_raw_parts(iid, 16) };
    let known = |which: u128| iid == iid_bytes(which);
    let handles = |obj: &'static ComObjHeader| -> bool {
        known(IID_IUNKNOWN)
            || obj.iids.iter().any(|&i| known(i))
    };
    // `this` punta al primo campo della struct (vtbl); l'header sta subito dopo.
    let header = unsafe { &*((this as *const u8).add(8) as *const ComObjHeader) };
    if handles(header) {
        unsafe { *out = this };
        S_OK
    } else {
        0x8000_4002u32 as i32 // E_NOINTERFACE
    }
}

/// I nostri oggetti vivono quanto il processo: AddRef/Release restituiscono un conteggio
/// fittizio e la memoria non viene mai liberata (il modulo ne conserva i puntatori).
unsafe extern "win64" fn unknown_add_ref(_this: *mut c_void) -> u32 {
    2
}

unsafe extern "win64" fn unknown_release(_this: *mut c_void) -> u32 {
    1
}

struct ComObjHeader {
    iids: &'static [u128],
}

// ---- IPropertyStore ----

#[repr(C)]
struct PropStore {
    vtbl: *const u8,
    header: ComObjHeader,
    name: &'static str,
    values: HashMap<(u128, u32), Value>,
}

unsafe extern "win64" fn ps_get_count(this: *mut c_void, count: *mut u32) -> i32 {
    let s = prop_store(this);
    unsafe { *count = s.values.len() as u32 };
    S_OK
}

unsafe extern "win64" fn ps_get_at(this: *mut c_void, i: u32, key: *mut u8) -> i32 {
    let s = prop_store(this);
    let Some((fmtid, pid)) = s.values.keys().nth(i as usize).copied() else {
        return E_POINTER;
    };
    unsafe {
        std::ptr::copy_nonoverlapping(iid_bytes(fmtid).as_ptr(), key, 16);
        *(key.add(16) as *mut u32) = pid;
    }
    S_OK
}

unsafe extern "win64" fn ps_get_value(this: *mut c_void, key: *const u8, pv: *mut u8) -> i32 {
    let s = prop_store(this);
    if key.is_null() || pv.is_null() {
        return E_POINTER;
    }
    let fmtid = iid_from_bytes(unsafe { &*(key as *const [u8; 16]) });
    let pid = unsafe { *(key.add(16) as *const u32) };
    match s.values.get(&(fmtid, pid)) {
        Some(v) => {
            trace!("{}: GetValue {} -> presente", s.name, key_str(fmtid, pid));
            unsafe { write_propvariant(pv, v) };
        }
        None => {
            trace!("{}: GetValue {} -> VT_EMPTY", s.name, key_str(fmtid, pid));
            unsafe { std::ptr::write_bytes(pv, 0, 0x18) };
        }
    }
    S_OK
}

unsafe extern "win64" fn ps_set_value(this: *mut c_void, key: *const u8, _pv: *const u8) -> i32 {
    let s = prop_store(this);
    if !key.is_null() {
        let fmtid = iid_from_bytes(unsafe { &*(key as *const [u8; 16]) });
        let pid = unsafe { *(key.add(16) as *const u32) };
        trace!("{}: SetValue {} ignorato", s.name, key_str(fmtid, pid));
    }
    S_OK
}

unsafe extern "win64" fn ps_commit(_this: *mut c_void) -> i32 {
    S_OK
}

unsafe fn prop_store(this: *mut c_void) -> &'static PropStore {
    unsafe { &*(this as *mut PropStore) }
}

#[repr(C)]
struct IPropertyStoreVtbl {
    query_interface: unsafe extern "win64" fn(*mut c_void, *const u8, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "win64" fn(*mut c_void) -> u32,
    release: unsafe extern "win64" fn(*mut c_void) -> u32,
    get_count: unsafe extern "win64" fn(*mut c_void, *mut u32) -> i32,
    get_at: unsafe extern "win64" fn(*mut c_void, u32, *mut u8) -> i32,
    get_value: unsafe extern "win64" fn(*mut c_void, *const u8, *mut u8) -> i32,
    set_value: unsafe extern "win64" fn(*mut c_void, *const u8, *const u8) -> i32,
    commit: unsafe extern "win64" fn(*mut c_void) -> i32,
}

static PROPERTYSTORE_VTABLE: IPropertyStoreVtbl = IPropertyStoreVtbl {
    query_interface: unknown_query_interface,
    add_ref: unknown_add_ref,
    release: unknown_release,
    get_count: ps_get_count,
    get_at: ps_get_at,
    get_value: ps_get_value,
    set_value: ps_set_value,
    commit: ps_commit,
};

fn leak_prop_store(name: &'static str, values: Vec<(PropKey, Value)>) -> *mut PropStore {
    let map = values.into_iter().map(|(k, v)| ((k.fmtid.0, k.pid), v)).collect();
    Box::into_raw(Box::new(PropStore {
        vtbl: std::ptr::addr_of!(PROPERTYSTORE_VTABLE) as *const u8,
        header: ComObjHeader { iids: &[IID_IPROPERTYSTORE] },
        name,
        values: map,
    }))
}

// ---- IAudioSystemEffectsPropertyStore ----

#[repr(C)]
struct FxStore {
    vtbl: *const u8,
    header: ComObjHeader,
    user: *mut PropStore,
}

unsafe fn fx_store(this: *mut c_void) -> &'static FxStore {
    unsafe { &*(this as *mut FxStore) }
}

unsafe extern "win64" fn fx_open(this: *mut c_void, _stgm: u32, out: *mut *mut c_void) -> i32 {
    trace!("IAudioSystemEffectsPropertyStore::Open*PropertyStore");
    unsafe { *out = fx_store(this).user as *mut c_void };
    S_OK
}

unsafe extern "win64" fn fx_open_user(this: *mut c_void, _stgm: u32, out: *mut *mut c_void) -> i32 {
    trace!("OpenUserPropertyStore");
    unsafe { *out = fx_store(this).user as *mut c_void };
    S_OK
}

unsafe extern "win64" fn fx_reset(_this: *mut c_void) -> i32 {
    S_OK
}

unsafe extern "win64" fn fx_register(_this: *mut c_void, _cb: *mut c_void) -> i32 {
    trace!("RegisterPropertyChangeNotification");
    S_OK
}

unsafe extern "win64" fn fx_unregister(_this: *mut c_void, _cb: *mut c_void) -> i32 {
    S_OK
}

#[repr(C)]
struct IAudioSystemEffectsPropertyStoreVtbl {
    query_interface: unsafe extern "win64" fn(*mut c_void, *const u8, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "win64" fn(*mut c_void) -> u32,
    release: unsafe extern "win64" fn(*mut c_void) -> u32,
    open_default_property_store: unsafe extern "win64" fn(*mut c_void, u32, *mut *mut c_void) -> i32,
    open_user_property_store: unsafe extern "win64" fn(*mut c_void, u32, *mut *mut c_void) -> i32,
    open_volatile_property_store: unsafe extern "win64" fn(*mut c_void, u32, *mut *mut c_void) -> i32,
    reset_user_property_store: unsafe extern "win64" fn(*mut c_void) -> i32,
    reset_volatile_property_store: unsafe extern "win64" fn(*mut c_void) -> i32,
    register_property_change_notification: unsafe extern "win64" fn(*mut c_void, *mut c_void) -> i32,
    unregister_property_change_notification: unsafe extern "win64" fn(*mut c_void, *mut c_void) -> i32,
}

static FXSTORE_VTABLE: IAudioSystemEffectsPropertyStoreVtbl = IAudioSystemEffectsPropertyStoreVtbl {
    query_interface: unknown_query_interface,
    add_ref: unknown_add_ref,
    release: unknown_release,
    open_default_property_store: fx_open,
    open_user_property_store: fx_open_user,
    open_volatile_property_store: fx_open,
    reset_user_property_store: fx_reset,
    reset_volatile_property_store: fx_reset,
    register_property_change_notification: fx_register,
    unregister_property_change_notification: fx_unregister,
};

// ---- IMMDevice ----

#[repr(C)]
struct Device {
    vtbl: *const u8,
    header: ComObjHeader,
    id: String,
    endpoint_props: *mut PropStore,
    fx: *mut FxStore,
}

unsafe fn device(this: *mut c_void) -> &'static Device {
    unsafe { &*(this as *mut Device) }
}

unsafe extern "win64" fn device_activate(this: *mut c_void, iid: *const u8, _clsctx: u32, _params: *const u8, out: *mut *mut c_void) -> i32 {
    let d = device(this);
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    let raw_iid = unsafe { &*(iid as *const [u8; 16]) };
    trace!("IMMDevice::Activate iid={:?}", os_dap_core::Guid(iid_from_bytes(raw_iid)));
    if *raw_iid == iid_bytes(IID_IAUDIOSYSTEMEFFECTSPROPERTYSTORE) {
        unsafe { *out = d.fx as *mut c_void };
        S_OK
    } else {
        E_NOTIMPL
    }
}

unsafe extern "win64" fn device_open_property_store(this: *mut c_void, _stgm: u32, out: *mut *mut c_void) -> i32 {
    trace!("IMMDevice::OpenPropertyStore");
    unsafe { *out = device(this).endpoint_props as *mut c_void };
    S_OK
}

unsafe extern "win64" fn device_get_id(this: *mut c_void, out: *mut *mut u16) -> i32 {
    unsafe { *out = co_wstr(&device(this).id) };
    S_OK
}

unsafe extern "win64" fn device_get_state(_this: *mut c_void, state: *mut u32) -> i32 {
    if !state.is_null() {
        unsafe { *state = 1 }; // DEVICE_STATE_ACTIVE
    }
    S_OK
}

#[repr(C)]
struct ImmDeviceVtbl {
    query_interface: unsafe extern "win64" fn(*mut c_void, *const u8, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "win64" fn(*mut c_void) -> u32,
    release: unsafe extern "win64" fn(*mut c_void) -> u32,
    activate: unsafe extern "win64" fn(*mut c_void, *const u8, u32, *const u8, *mut *mut c_void) -> i32,
    open_property_store: unsafe extern "win64" fn(*mut c_void, u32, *mut *mut c_void) -> i32,
    get_id: unsafe extern "win64" fn(*mut c_void, *mut *mut u16) -> i32,
    get_state: unsafe extern "win64" fn(*mut c_void, *mut u32) -> i32,
}

static DEVICE_VTABLE: ImmDeviceVtbl = ImmDeviceVtbl {
    query_interface: unknown_query_interface,
    add_ref: unknown_add_ref,
    release: unknown_release,
    activate: device_activate,
    open_property_store: device_open_property_store,
    get_id: device_get_id,
    get_state: device_get_state,
};

// ---- IMMDeviceCollection ----

#[repr(C)]
struct DeviceCollection {
    vtbl: *const u8,
    header: ComObjHeader,
    device: *mut Device,
}

pub(crate) unsafe extern "win64" fn dc_get_count(_this: *mut c_void, out: *mut u32) -> i32 {
    trace!("IMMDeviceCollection::GetCount");
    if !out.is_null() {
        unsafe { *out = 1 };
    }
    S_OK
}

unsafe extern "win64" fn dc_item(this: *mut c_void, n: u32, out: *mut *mut c_void) -> i32 {
    trace!("IMMDeviceCollection::Item({n})");
    if n == 0 && !out.is_null() {
        unsafe { *out = (*(this as *mut DeviceCollection)).device as *mut c_void };
        S_OK
    } else {
        E_POINTER
    }
}

#[repr(C)]
struct ImmDeviceCollectionVtbl {
    query_interface: unsafe extern "win64" fn(*mut c_void, *const u8, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "win64" fn(*mut c_void) -> u32,
    release: unsafe extern "win64" fn(*mut c_void) -> u32,
    get_count: unsafe extern "win64" fn(*mut c_void, *mut u32) -> i32,
    item: unsafe extern "win64" fn(*mut c_void, u32, *mut *mut c_void) -> i32,
}

static COLLECTION_VTABLE: ImmDeviceCollectionVtbl = ImmDeviceCollectionVtbl {
    query_interface: unknown_query_interface,
    add_ref: unknown_add_ref,
    release: unknown_release,
    get_count: dc_get_count,
    item: dc_item,
};

// ---- grafo dell'endpoint virtuale ----

/// Tutto ciò che il modulo riceve da SetAPOInitParameters. Gli oggetti sono leak:
/// il modulo ne conserva i puntatori per la vita dell'istanza.
pub struct Endpoint {
    pub endpoint_props: usize,
    pub collection: usize,
}

pub fn make_endpoint(setup: &ModuleSetup) -> Endpoint {
    let endpoint_props = leak_prop_store("endpoint", setup.endpoint_props.clone());
    let user = leak_prop_store("fx-user", setup.fx_props.clone());
    let fx = Box::into_raw(Box::new(FxStore {
        vtbl: std::ptr::addr_of!(FXSTORE_VTABLE) as *const u8,
        header: ComObjHeader { iids: &[IID_IAUDIOSYSTEMEFFECTSPROPERTYSTORE] },
        user,
    }));
    let device = Box::into_raw(Box::new(Device {
        vtbl: std::ptr::addr_of!(DEVICE_VTABLE) as *const u8,
        header: ComObjHeader { iids: &[IID_IMMDEVICE] },
        id: setup.device_id.clone(),
        endpoint_props,
        fx,
    }));
    let collection = Box::into_raw(Box::new(DeviceCollection {
        vtbl: std::ptr::addr_of!(COLLECTION_VTABLE) as *const u8,
        header: ComObjHeader { iids: &[IID_IMMDEVICECOLLECTION] },
        device,
    }));
    Endpoint { endpoint_props: endpoint_props as usize, collection: collection as usize }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propvariant_blob_layout() {
        let mut pv = [0u8; 24];
        unsafe { write_propvariable(pv.as_mut_ptr(), &Value::Blob(vec![1, 2, 3, 4])) };
        assert_eq!(u16::from_le_bytes(pv[0..2].try_into().unwrap()), VT_BLOB);
        assert_eq!(u32::from_le_bytes(pv[8..12].try_into().unwrap()), 4);
        let data = unsafe { std::slice::from_raw_parts(*(pv.as_ptr().add(0x10) as *const *const u8), 4) };
        assert_eq!(data, &[1, 2, 3, 4]);
    }

    unsafe fn write_propvariable(pv: *mut u8, v: &Value) {
        unsafe { write_propvariant(pv, v) }
    }

    #[test]
    fn iids_match_sdk_headers() {
        let text = |u: u128| format!("{:?}", os_dap_core::Guid(u));
        assert_eq!(text(IID_IMMDEVICE), "D666063F-1587-4E43-81F1-B948E807363F");
        assert_eq!(text(IID_IMMDEVICECOLLECTION), "0BD7A1BE-7A1A-44DB-8397-CC5392387B5E");
        assert_eq!(text(IID_IPROPERTYSTORE), "886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99");
        assert_eq!(text(IID_IAUDIOSYSTEMEFFECTSPROPERTYSTORE), "302AE7F9-D7E0-43E4-971B-1F8293613D2A");
        assert_eq!(text(IID_IUNKNOWN), "00000000-0000-0000-C000-000000000046");
    }

    /// QueryInterface di un oggetto (slot 0) con l'IID dato.
    unsafe fn qi(obj: *mut c_void, iid: u128) -> (i32, *mut c_void) {
        let mut out: *mut c_void = std::ptr::null_mut();
        let hr = unsafe {
            let vtbl = *(obj as *const *const usize);
            let f: unsafe extern "win64" fn(*mut c_void, *const u8, *mut *mut c_void) -> i32 = std::mem::transmute(*vtbl);
            f(obj, iid_bytes(iid).as_ptr(), &mut out)
        };
        (hr, out)
    }

    #[test]
    fn every_object_answers_its_own_iid() {
        let setup = ModuleSetup::new(std::path::Path::new("x.dll"), os_dap_core::Mode::Headphones, 256, Vec::new(), false).unwrap();
        let ep = make_endpoint(&setup);
        let collection = ep.collection as *mut c_void;
        let device = unsafe { (*(collection as *mut DeviceCollection)).device } as *mut c_void;
        let fx = unsafe { (*(device as *mut Device)).fx } as *mut c_void;
        let user = unsafe { (*(fx as *mut FxStore)).user } as *mut c_void;
        let props = ep.endpoint_props as *mut c_void;
        for (obj, iid) in [
            (collection, IID_IMMDEVICECOLLECTION),
            (device, IID_IMMDEVICE),
            (fx, IID_IAUDIOSYSTEMEFFECTSPROPERTYSTORE),
            (user, IID_IPROPERTYSTORE),
            (props, IID_IPROPERTYSTORE),
        ] {
            assert_eq!(unsafe { qi(obj, iid) }, (S_OK, obj), "QI({:?})", os_dap_core::Guid(iid));
            assert_eq!(unsafe { qi(obj, IID_IUNKNOWN) }, (S_OK, obj));
        }
        // e non risponde a quelli degli altri
        assert_eq!(unsafe { qi(device, IID_IMMDEVICECOLLECTION) }.0, 0x8000_4002u32 as i32);
        assert_eq!(unsafe { qi(collection, IID_IMMDEVICE) }.1, std::ptr::null_mut());
    }
}
