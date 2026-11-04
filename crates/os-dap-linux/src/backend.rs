//! Backend Linux: carica `DolbyAudioProcessing.dll` (DAP 7.3.7) con il loader PE nativo e lo
//! pilota tramite IDAPModule2, replicando la sequenza dello shell Microsoft `DolbyHrtfEnc.dll`
//! (doc sez. 12.6-12.7), esattamente come il backend Windows (`os-dap-host`): stessi dati
//! (da `os-dap-core`), stesse vtable, ABI win64.

use std::ffi::c_void;

use anyhow::{bail, Context, Result};
use os_dap_core::abi::EncodeInput;
use os_dap_core::{Backend, ModuleSetup};
use os_pe_loader::shims::com::guid_bytes;
use os_pe_loader::shims::winrt::HStringRef;
use os_pe_loader::Image;

use crate::com;

/// AUDIO_SIGNALPROCESSINGMODE_DEFAULT (ksmedia.h).
const AUDIO_SIGNALPROCESSINGMODE_DEFAULT: u128 = 0xc18e2f7e_933d_4965_b7d1_1eef228d2af3;

pub const IID_IDAPMODULE2: u128 = 0x2b2e53bc_651b_4a90_8f64_38531c154fda;
/// Interfaccia ausiliaria: slot 3 = SetAPOInitParameters(APOInitSystemEffects3*), lo stesso
/// IID di os-dap-host. La catena di QueryInterface del modulo (RVA […]) restituisce,
/// rispetto a IDAPModule2: b5bb3cae → +0x8 (slot 3 = GetStereoBypassAllowed, quella che lo
/// shell usa a RVA […]), 67b16434 → +0x10 (SetAPOInitParameters), 56b0c76f → +0x18
/// (GetApoNotificationRegistrationInfo). Verificato su Windows il 3/11/2026 (doc sez. 12.12).
pub const IID_DAP_APO_INIT: u128 = 0x67b16434_1ef3_47bc_8d2b_f485ee148c32;

/// APOInitSystemEffects3 di audioapovdd/o (il modulo legge [3] e [5]: proprietà endpoint
/// e IMMDeviceCollection). Stessa forma di `windows::Win32::Media::Audio::Apo::…`.
#[repr(C)]
struct ApoInitSystemEffects3 {
    cb_size: u32, // +0
    clsid: [u8; 16], // +4
    endpoint_props: usize, // +24
    service_provider: usize, // +32
    device_collection: usize, // +40
    n_software_io_device_in_collection: u32, // +48
    n_software_io_connector_index: u32, // +52
    audio_processing_mode: [u8; 16], // +56
    initialize_for_discovery_only: i32, // +72
}
const _: () = assert!(std::mem::size_of::<ApoInitSystemEffects3>() == 80);

type FnDllGetActivationFactory = unsafe extern "win64" fn(u64, *mut *mut c_void) -> i32;

/// Slot di una vtable COM, con ABI win64.
unsafe fn vslot<F: Copy>(obj: *mut c_void, slot: usize) -> F {
    unsafe {
        let vtbl = *(obj as *const *const usize);
        std::mem::transmute_copy(&*vtbl.add(slot))
    }
}

fn check(hr: i32, what: &str) -> Result<()> {
    if hr < 0 {
        bail!("{what} fallita: HRESULT {hr:#010x}");
    }
    Ok(())
}

pub struct LinuxBackend {
    module: *mut c_void,
    encoder_config: [u32; 8],
    /// Nome della classe WinRT: i dati UTF-16 devono vivere quanto l'HSTRING che passiamo.
    _class_name: HStringRef,
    _endpoint: com::Endpoint,
}

impl Backend for LinuxBackend {
    fn open(setup: &ModuleSetup) -> Result<Self> {
        if setup.verbose {
            com::VERBOSE.store(true, std::sync::atomic::Ordering::Relaxed);
            os_pe_loader::set_verbose(true);
        }
        os_pe_loader::install_thread();

        let image = Image::load(setup.dll).with_context(|| format!("caricamento {}", setup.dll.display()))?;
        let endpoint = com::make_endpoint(setup);

        let get_factory_addr = image
            .export("DllGetActivationFactory")
            .ok_or_else(|| anyhow::anyhow!("DllGetActivationFactory non esportata"))?;
        let get_factory: FnDllGetActivationFactory = unsafe { std::mem::transmute(get_factory_addr) };

        let class_name = HStringRef::new("Dolby.DolbyAudioProcessingModule");
        let mut factory: *mut c_void = std::ptr::null_mut();
        unsafe { check(get_factory(class_name.handle(), &mut factory), "DllGetActivationFactory") }?;

        let mut module: *mut c_void = std::ptr::null_mut();
        unsafe {
            // IActivationFactory::ActivateInstance = slot 6.
            let activate: unsafe extern "win64" fn(*mut c_void, *mut *mut c_void) -> i32 = vslot(factory, 6);
            let mut inst: *mut c_void = std::ptr::null_mut();
            check(activate(factory, &mut inst), "IActivationFactory::ActivateInstance")?;

            let qi: unsafe extern "win64" fn(*mut c_void, *const u8, *mut *mut c_void) -> i32 = vslot(inst, 0);
            check(qi(inst, guid_bytes(IID_IDAPMODULE2).as_ptr(), &mut module), "QueryInterface(IDAPModule2)")?;

            // Slot 3: Initialize(pAudioDeviceModulesManager, tracer): entrambi opzionali.
            let init: unsafe extern "win64" fn(*mut c_void, *mut c_void, *mut c_void) -> i32 = vslot(module, 3);
            check(init(module, std::ptr::null_mut(), std::ptr::null_mut()), "IDAPModule2::Initialize")?;

            // Interfaccia ausiliaria, slot 3: SetAPOInitParameters.
            // Con il QueryInterface di IDAPModule2 (il suo slot 0 riporta `this` all'inizio
            // dell'oggetto), non con quello di `inst`: fino alla fase 3-bis qui c'era `qi`, che
            // chiamato con `module` vede l'oggetto spostato di 8 byte e risponde con
            // l'interfaccia successiva (b5bb3cae arrivava a SetAPOInitParameters per questo).
            let module_qi: unsafe extern "win64" fn(*mut c_void, *const u8, *mut *mut c_void) -> i32 = vslot(module, 0);
            let mut aux: *mut c_void = std::ptr::null_mut();
            check(module_qi(module, guid_bytes(IID_DAP_APO_INIT).as_ptr(), &mut aux), "QueryInterface(APO init)")?;
            if setup.verbose {
                // Stessa riga di os-dap-host: atteso +0x10, vtable […], slot 3 […].
                let vtbl = *(aux as *const *const usize);
                eprintln!(
                    "[qi] APO init: this{:+#x}, vtable RVA {:#x}, slot 3 RVA {:#x}",
                    aux as isize - module as isize,
                    vtbl as usize - image.base,
                    *vtbl.add(3) - image.base
                );
            }
            let apo = ApoInitSystemEffects3 {
                cb_size: std::mem::size_of::<ApoInitSystemEffects3>() as u32,
                clsid: [0; 16],
                endpoint_props: endpoint.endpoint_props,
                service_provider: 0,
                device_collection: endpoint.collection,
                n_software_io_device_in_collection: 0,
                n_software_io_connector_index: 0,
                audio_processing_mode: guid_bytes(AUDIO_SIGNALPROCESSINGMODE_DEFAULT),
                initialize_for_discovery_only: 0,
            };
            let set_apo: unsafe extern "win64" fn(*mut c_void, *const c_void) -> i32 = vslot(aux, 3);
            check(set_apo(aux, &apo as *const _ as *const c_void), "SetAPOInitParameters")?;

            // Slot 4: ConfigureEncoder(cfg[8], 32, nullptr).
            let configure: unsafe extern "win64" fn(*mut c_void, *const u32, u32, *const u16) -> i32 = vslot(module, 4);
            let cfg = &setup.encoder_config;
            check(
                configure(module, cfg.as_ptr(), std::mem::size_of_val(cfg) as u32, std::ptr::null()),
                "IDAPModule2::ConfigureEncoder",
            )?;
        }

        eprintln!(
            "modulo DAP caricato con il loader PE: base {:#x}, {} export",
            image.base,
            image.export_count()
        );
        Ok(LinuxBackend { module, encoder_config: setup.encoder_config, _class_name: class_name, _endpoint: endpoint })
    }

    unsafe fn encode(&mut self, input: *mut EncodeInput, out: *mut f32) -> Result<()> {
        // Il thread che elabora può non essere quello di `open` (fase 4: il thread di
        // processing di PipeWire): senza TEB il primo gs:[0x58] del modulo va in SIGSEGV.
        // Idempotente e senza allocazioni dopo la prima volta.
        os_pe_loader::install_thread();
        let encode: unsafe extern "win64" fn(*mut c_void, *mut EncodeInput, *mut f32) -> i32 = unsafe { vslot(self.module, 5) };
        unsafe { check(encode(self.module, input, out), "IDAPModule2::EncodeAudioData") }
    }

    fn reconfigure(&mut self) -> Result<()> {
        os_pe_loader::install_thread();
        let cfg = &self.encoder_config;
        unsafe {
            let configure: unsafe extern "win64" fn(*mut c_void, *const u32, u32, *const u16) -> i32 = vslot(self.module, 4);
            check(
                configure(self.module, cfg.as_ptr(), std::mem::size_of_val(cfg) as u32, std::ptr::null()),
                "IDAPModule2::ConfigureEncoder (riconfigurazione)",
            )
        }
    }
}

// Il modulo possiede i propri oggetti COM: `LinuxBackend` non implementa Drop perché il
// processo dura quanto l'elaborazione e gli oggetti sono leak intenzionali.
