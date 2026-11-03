//! Backend Windows: carica `DolbyAudioProcessing.dll` (DAP 7.3.7) con LoadLibraryExW e lo pilota
//! tramite IDAPModule2, replicando la sequenza dello shell Microsoft `DolbyHrtfEnc.dll`
//! (doc sez. 12.6-12.7). I dati passati al modulo vengono tutti da os-dap-core.

use std::ffi::c_void;
use std::sync::atomic::Ordering;

use anyhow::{anyhow, bail, Context, Result};
use os_dap_core::abi::EncodeInput;
use os_dap_core::{Backend, ModuleSetup};
use windows::Win32::Media::Audio::Apo::{APOInitBaseStruct, APOInitSystemEffects3};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
};
use windows::Win32::System::WinRT::IActivationFactory;
use windows_core::{s, IUnknown, Interface, GUID, HRESULT, HSTRING, PCWSTR};

use crate::stubs::{self, Endpoint};
use crate::{debuglog, iathook};

/// AUDIO_SIGNALPROCESSINGMODE_DEFAULT (ksmedia.h).
const AUDIO_SIGNALPROCESSINGMODE_DEFAULT: GUID = GUID::from_u128(0xc18e2f7e_933d_4965_b7d1_1eef228d2af3);

pub const IID_IDAPMODULE2: GUID = GUID::from_u128(0x2b2e53bc_651b_4a90_8f64_38531c154fda);
/// Interfaccia ausiliaria: slot 3 = SetAPOInitParameters(APOInitSystemEffects3*).
pub const IID_DAP_APO_INIT: GUID = GUID::from_u128(0x67b16434_1ef3_47bc_8d2b_f485ee148c32);

type FnDllGetActivationFactory = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT;
type FnInitialize = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT;
type FnSetApoInit = unsafe extern "system" fn(*mut c_void, *const APOInitSystemEffects3) -> HRESULT;
type FnConfigureEncoder = unsafe extern "system" fn(*mut c_void, *const u32, u32, PCWSTR) -> HRESULT;
type FnEncodeAudioData = unsafe extern "system" fn(*mut c_void, *mut EncodeInput, *mut f32) -> HRESULT;

unsafe fn vslot<F: Copy>(obj: &IUnknown, slot: usize) -> F {
    let vtbl = *(obj.as_raw() as *const *const usize);
    let f = *vtbl.add(slot);
    std::mem::transmute_copy(&f)
}

fn check(hr: HRESULT, what: &str) -> Result<()> {
    if hr.is_err() {
        bail!("{what} fallita: HRESULT {:#010x} ({})", hr.0 as u32, hr.message());
    }
    Ok(())
}

pub struct WindowsBackend {
    module: IUnknown,
    // Tenuto vivo finché il modulo può usarlo.
    _endpoint: Endpoint,
}

impl Backend for WindowsBackend {
    fn open(setup: &ModuleSetup) -> Result<Self> {
        if setup.verbose {
            stubs::VERBOSE.store(true, Ordering::Relaxed);
            debuglog::install();
        }
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().context("CoInitializeEx")?;
        let endpoint = stubs::make_endpoint(setup);

        let dll = setup.dll.canonicalize().with_context(|| format!("DLL non trovato: {}", setup.dll.display()))?;
        let module = unsafe {
            let hmod = LoadLibraryExW(
                &HSTRING::from(dll.as_os_str()),
                None,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
            )
            .with_context(|| format!("LoadLibraryExW({})", dll.display()))?;
            if iathook::install(hmod.0, setup.verbose) < 2 {
                bail!("hook IAT non installati");
            }
            let proc = GetProcAddress(hmod, s!("DllGetActivationFactory"))
                .ok_or_else(|| anyhow!("DllGetActivationFactory non esportata"))?;
            let get_factory: FnDllGetActivationFactory = std::mem::transmute(proc);

            let class = HSTRING::from("Dolby.DolbyAudioProcessingModule");
            let mut factory: *mut c_void = std::ptr::null_mut();
            check(get_factory(std::mem::transmute_copy(&class), &mut factory), "DllGetActivationFactory")?;
            let factory = IActivationFactory::from_raw(factory);
            let inst = factory.ActivateInstance().context("ActivateInstance")?;
            let mut m: *mut c_void = std::ptr::null_mut();
            check(inst.query(&IID_IDAPMODULE2, &mut m), "QueryInterface(IDAPModule2)")?;
            IUnknown::from_raw(m)
        };

        unsafe {
            // Slot 3: Initialize(pAudioDeviceModulesManager, tracer): entrambi opzionali.
            let init: FnInitialize = vslot(&module, 3);
            check(init(module.as_raw(), std::ptr::null_mut(), std::ptr::null_mut()), "IDAPModule2::Initialize")?;

            // Interfaccia ausiliaria, slot 3: SetAPOInitParameters.
            let mut aux: *mut c_void = std::ptr::null_mut();
            check(module.query(&IID_DAP_APO_INIT, &mut aux), "QueryInterface(APO init)")?;
            let aux = IUnknown::from_raw(aux);
            let apo = APOInitSystemEffects3 {
                APOInit: APOInitBaseStruct {
                    cbSize: std::mem::size_of::<APOInitSystemEffects3>() as u32,
                    clsid: GUID::zeroed(),
                },
                pAPOEndpointProperties: std::mem::ManuallyDrop::new(Some(endpoint.endpoint_props.clone())),
                pServiceProvider: std::mem::ManuallyDrop::new(None),
                pDeviceCollection: std::mem::ManuallyDrop::new(Some(endpoint.collection.clone())),
                nSoftwareIoDeviceInCollection: 0,
                nSoftwareIoConnectorIndex: 0,
                AudioProcessingMode: AUDIO_SIGNALPROCESSINGMODE_DEFAULT,
                InitializeForDiscoveryOnly: false.into(),
            };
            let set_apo: FnSetApoInit = vslot(&aux, 3);
            let hr = set_apo(aux.as_raw(), &apo);
            std::mem::ManuallyDrop::into_inner(apo.pAPOEndpointProperties);
            std::mem::ManuallyDrop::into_inner(apo.pDeviceCollection);
            check(hr, "SetAPOInitParameters")?;

            // Slot 4: ConfigureEncoder(cfg[8], 32, nullptr).
            let cfg = &setup.encoder_config;
            let configure: FnConfigureEncoder = vslot(&module, 4);
            check(
                configure(module.as_raw(), cfg.as_ptr(), std::mem::size_of_val(cfg) as u32, PCWSTR::null()),
                "IDAPModule2::ConfigureEncoder",
            )?;
        }

        Ok(WindowsBackend { module, _endpoint: endpoint })
    }

    unsafe fn encode(&mut self, input: *mut EncodeInput, out: *mut f32) -> Result<()> {
        let encode: FnEncodeAudioData = vslot(&self.module, 5);
        check(encode(self.module.as_raw(), input, out), "IDAPModule2::EncodeAudioData")
    }
}
