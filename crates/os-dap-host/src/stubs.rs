//! Oggetti COM che il modulo DAP si aspetta da Windows (doc sez. 12.7):
//! IMMDeviceCollection → IMMDevice → IAudioSystemEffectsPropertyStore → IPropertyStore,
//! più l'IPropertyStore dell'endpoint. Tutto in memoria, niente MMDevAPI.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{E_NOTIMPL, E_POINTER, PROPERTYKEY};
use windows::Win32::Media::Audio::{
    IAudioSystemEffectsPropertyChangeNotificationClient, IAudioSystemEffectsPropertyStore,
    IAudioSystemEffectsPropertyStore_Impl, IMMDevice, IMMDeviceCollection,
    IMMDeviceCollection_Impl, IMMDevice_Impl, DEVICE_STATE, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoTaskMemAlloc, CLSCTX, STGM};
use windows::Win32::System::Variant::{VT_BLOB, VT_LPWSTR};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, IPropertyStore_Impl};
use os_dap_core::{ModuleSetup, PropKey, Value};
use windows_core::{implement, IUnknown, Interface, Ref, Result, GUID, PWSTR};

pub static VERBOSE: AtomicBool = AtomicBool::new(false);

macro_rules! trace {
    ($($arg:tt)*) => {
        if VERBOSE.load(Ordering::Relaxed) {
            eprintln!("[stub] {}", format!($($arg)*));
        }
    };
}

fn co_alloc_copy(bytes: &[u8]) -> Result<*mut u8> {
    let p = unsafe { CoTaskMemAlloc(bytes.len().max(1)) } as *mut u8;
    if p.is_null() {
        return Err(E_POINTER.into());
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len()) };
    Ok(p)
}

fn co_wstr(s: &str) -> Result<PWSTR> {
    let w: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2) };
    Ok(PWSTR(co_alloc_copy(bytes)? as *mut u16))
}

/// Costruisce un PROPVARIANT di proprietà del chiamante (che farà PropVariantClear).
fn to_propvariant(v: &Value) -> Result<PROPVARIANT> {
    let mut pv = PROPVARIANT::default();
    unsafe {
        let inner = &mut pv.Anonymous.Anonymous;
        match v {
            Value::Blob(b) => {
                inner.vt = VT_BLOB;
                inner.Anonymous.blob.cbSize = b.len() as u32;
                inner.Anonymous.blob.pBlobData = co_alloc_copy(b)?;
            }
            Value::Str(s) => {
                inner.vt = VT_LPWSTR;
                inner.Anonymous.pwszVal = co_wstr(s)?;
            }
        }
    }
    Ok(pv)
}

fn key_str(k: &PROPERTYKEY) -> String {
    format!("{{{:?}}},{}", k.fmtid, k.pid)
}

#[implement(IPropertyStore)]
pub struct PropStore {
    name: &'static str,
    values: HashMap<(GUID, u32), Value>,
}

impl PropStore {
    pub fn new(name: &'static str, values: &[(PropKey, Value)]) -> IPropertyStore {
        let values = values.iter().map(|(k, v)| ((GUID::from_u128(k.fmtid.0), k.pid), v.clone())).collect();
        PropStore { name, values }.into()
    }
}

impl IPropertyStore_Impl for PropStore_Impl {
    fn GetCount(&self) -> Result<u32> {
        Ok(self.values.len() as u32)
    }

    fn GetAt(&self, iprop: u32, pkey: *mut PROPERTYKEY) -> Result<()> {
        let (fmtid, pid) = *self.values.keys().nth(iprop as usize).ok_or(E_POINTER)?;
        unsafe { *pkey = PROPERTYKEY { fmtid, pid } };
        Ok(())
    }

    fn GetValue(&self, key: *const PROPERTYKEY) -> Result<PROPVARIANT> {
        let key = unsafe { key.as_ref() }.ok_or(E_POINTER)?;
        match self.values.get(&(key.fmtid, key.pid)) {
            Some(v) => {
                trace!("{}: GetValue {} -> presente", self.name, key_str(key));
                to_propvariant(v)
            }
            None => {
                trace!("{}: GetValue {} -> VT_EMPTY", self.name, key_str(key));
                Ok(PROPVARIANT::default())
            }
        }
    }

    fn SetValue(&self, key: *const PROPERTYKEY, _propvar: *const PROPVARIANT) -> Result<()> {
        if let Some(key) = unsafe { key.as_ref() } {
            trace!("{}: SetValue {} ignorato", self.name, key_str(key));
        }
        Ok(())
    }

    fn Commit(&self) -> Result<()> {
        Ok(())
    }
}

#[implement(IAudioSystemEffectsPropertyStore)]
pub struct FxPropertyStore {
    user: IPropertyStore,
}

impl IAudioSystemEffectsPropertyStore_Impl for FxPropertyStore_Impl {
    fn OpenDefaultPropertyStore(&self, _stgm: u32) -> Result<IPropertyStore> {
        trace!("OpenDefaultPropertyStore");
        Ok(self.user.clone())
    }

    fn OpenUserPropertyStore(&self, _stgm: u32) -> Result<IPropertyStore> {
        trace!("OpenUserPropertyStore");
        Ok(self.user.clone())
    }

    fn OpenVolatilePropertyStore(&self, _stgm: u32) -> Result<IPropertyStore> {
        trace!("OpenVolatilePropertyStore");
        Ok(self.user.clone())
    }

    fn ResetUserPropertyStore(&self) -> Result<()> {
        Ok(())
    }

    fn ResetVolatilePropertyStore(&self) -> Result<()> {
        Ok(())
    }

    fn RegisterPropertyChangeNotification(
        &self,
        _cb: Ref<IAudioSystemEffectsPropertyChangeNotificationClient>,
    ) -> Result<()> {
        trace!("RegisterPropertyChangeNotification");
        Ok(())
    }

    fn UnregisterPropertyChangeNotification(
        &self,
        _cb: Ref<IAudioSystemEffectsPropertyChangeNotificationClient>,
    ) -> Result<()> {
        Ok(())
    }
}

#[implement(IMMDevice)]
pub struct Device {
    id: String,
    endpoint_props: IPropertyStore,
    fx: IAudioSystemEffectsPropertyStore,
}

impl IMMDevice_Impl for Device_Impl {
    fn Activate(
        &self,
        iid: *const GUID,
        clsctx: CLSCTX,
        _params: *const PROPVARIANT,
        ppinterface: *mut *mut c_void,
    ) -> Result<()> {
        let iid = unsafe { iid.as_ref() }.ok_or(E_POINTER)?;
        trace!("IMMDevice::Activate iid={iid:?} clsctx={:#x}", clsctx.0);
        if *iid != IAudioSystemEffectsPropertyStore::IID {
            return Err(E_NOTIMPL.into());
        }
        let unk: IUnknown = self.fx.cast()?;
        unsafe { unk.query(iid, ppinterface).ok() }
    }

    fn OpenPropertyStore(&self, _stgm: STGM) -> Result<IPropertyStore> {
        trace!("IMMDevice::OpenPropertyStore");
        Ok(self.endpoint_props.clone())
    }

    fn GetId(&self) -> Result<PWSTR> {
        co_wstr(&self.id)
    }

    fn GetState(&self) -> Result<DEVICE_STATE> {
        Ok(DEVICE_STATE_ACTIVE)
    }
}

#[implement(IMMDeviceCollection)]
pub struct DeviceCollection {
    device: IMMDevice,
}

impl IMMDeviceCollection_Impl for DeviceCollection_Impl {
    fn GetCount(&self) -> Result<u32> {
        trace!("IMMDeviceCollection::GetCount");
        Ok(1)
    }

    fn Item(&self, n: u32) -> Result<IMMDevice> {
        trace!("IMMDeviceCollection::Item({n})");
        if n == 0 {
            Ok(self.device.clone())
        } else {
            Err(E_POINTER.into())
        }
    }
}

/// Endpoint virtuale: proprietà dell'endpoint + property store FX con i blob Dolby.
pub struct Endpoint {
    pub endpoint_props: IPropertyStore,
    pub collection: IMMDeviceCollection,
}

pub fn make_endpoint(setup: &ModuleSetup) -> Endpoint {
    let endpoint_props = PropStore::new("endpoint", &setup.endpoint_props);
    let user = PropStore::new("fx-user", &setup.fx_props);
    let fx: IAudioSystemEffectsPropertyStore = FxPropertyStore { user }.into();
    let device: IMMDevice = Device { id: setup.device_id.clone(), endpoint_props: endpoint_props.clone(), fx }.into();
    let collection: IMMDeviceCollection = DeviceCollection { device }.into();
    Endpoint { endpoint_props, collection }
}

#[cfg(test)]
mod tests {
    use os_dap_core::props::{PKEY_AUDIOENDPOINT_GUID, PKEY_DEVICE_FRIENDLYNAME};
    use windows::Win32::Devices::Properties::DEVPKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::PKEY_AudioEndpoint_GUID;

    use super::*;

    /// Le chiavi e la forma dei GUID del nucleo coincidono con quelle dell'SDK di Windows.
    #[test]
    fn core_keys_match_sdk() {
        let k = |p: PropKey| (GUID::from_u128(p.fmtid.0), p.pid);
        assert_eq!(k(PKEY_AUDIOENDPOINT_GUID), (PKEY_AudioEndpoint_GUID.fmtid, PKEY_AudioEndpoint_GUID.pid));
        assert_eq!(k(PKEY_DEVICE_FRIENDLYNAME), (DEVPKEY_Device_FriendlyName.fmtid, DEVPKEY_Device_FriendlyName.pid));
        let g = os_dap_core::props::ENDPOINT_GUID;
        assert_eq!(format!("{g:?}"), format!("{:?}", GUID::from_u128(g.0)));
    }
}
