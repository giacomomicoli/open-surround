//! Implementazioni sostitutive delle API Windows importate dal modulo DAP: tutto ciò che il
//! DLL chiede al sistema passa da qui, con ABI win64, e viene eseguito da codice Rust.
//!
//! `resolve` associa (DLL, nome o ordinale) → indirizzo della funzione shim; serve sia al
//! loader (riempimento della IAT) sia a `GetProcAddress` (il CRT del modulo risolve per
//! nome funzioni come FlsAlloc e MessageBoxW).

pub mod com;
mod crt;
pub mod etw;
pub mod handles;
pub mod heap;
mod libloader;
mod mm;
mod sync;
pub mod winrt;

use crate::pe::ImportName;

pub use handles::{thread_exit_code, THREADS};

/// Ultimo shim mancante richiamato: stampato e poi abort (si arriva una volta sola).
static MISSING: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Shim segnaposto per un import non implementato: fallisce forte alla prima chiamata,
/// con il nome di chi mancava, invece di un crash obscuro dentro il modulo.
pub fn missing_stub(dll: String, name: ImportName) -> usize {
    let what = format!("{dll}!{}", name.describe());
    eprintln!("[os-pe-loader] import non implementato: {what} (abort alla prima chiamata)");
    *MISSING.lock().unwrap() = Some(what);
    (missing_trampoline as unsafe extern "win64" fn() -> !) as usize
}

pub(crate) unsafe extern "win64" fn missing_trampoline() -> ! {
    let what = MISSING.lock().unwrap().clone().unwrap_or_else(|| "sconosciuto".into());
    eprintln!("[os-pe-loader] chiamata a un'API non implementata: {what}");
    std::process::abort();
}

/// Nome normalizzato della DLL di import (minuscolo, senza percorso).
fn dll_key(dll: &str) -> String {
    let name = dll.rsplit(['\\', '/']).next().unwrap_or(dll);
    name.to_ascii_lowercase()
}



/// Associa un import a uno shim. `dll` arriva dall'intestazione del PE, `name` dalla thunk.
pub fn resolve(dll: &str, name: &ImportName) -> Option<usize> {
    let dll = dll_key(dll);
    match name {
        ImportName::Name(n) => resolve_named(&dll, n),
        ImportName::Ordinal(o) => match (dll.as_str(), *o) {
            ("oleaut32.dll", 2) => Some(crate::shims::com::oleaut32_sys_alloc_string as usize),
            ("oleaut32.dll", 6) => Some(crate::shims::com::oleaut32_sys_free_string as usize),
            ("oleaut32.dll", 7) => Some(crate::shims::com::oleaut32_sys_string_len as usize),
            ("oleaut32.dll", 200) => Some(crate::shims::com::oleaut32_get_error_info as usize),
            ("oleaut32.dll", 201) => Some(crate::shims::com::oleaut32_set_error_info as usize),
            _ => None,
        },
    }
}

fn resolve_named(_dll: &str, name: &str) -> Option<usize> {
    // Il nome della DLL non discrimina: gli API-set portano tutti qui e i nomi di funzione
    // sono univoci nell'insieme importato dal modulo (verificato sul DLL di DAP 7.3.7).
    resolve_any(name)
}

fn resolve_any(name: &str) -> Option<usize> {
    Some(match name {
        // heap e allocazioni
        "HeapAlloc" => heap::heap_alloc as usize,
        "HeapFree" => heap::heap_free as usize,
        "HeapReAlloc" => heap::heap_realloc as usize,
        "HeapSize" => heap::heap_size as usize,
        "GetProcessHeap" => heap::get_process_heap as usize,
        "CoTaskMemAlloc" => heap::co_task_mem_alloc as usize,
        "CoTaskMemFree" => heap::co_task_mem_free as usize,
        "LocalAlloc" => heap::local_alloc as usize,
        "LocalFree" => heap::local_free as usize,

        // sincronizzazione embedded (SRW, critical section, condition variable, init-once)
        "AcquireSRWLockExclusive" => sync::acquire_srw_exclusive as usize,
        "TryAcquireSRWLockExclusive" => sync::try_acquire_srw_exclusive as usize,
        "ReleaseSRWLockExclusive" => sync::release_srw_exclusive as usize,
        "AcquireSRWLockShared" => sync::acquire_srw_shared as usize,
        "ReleaseSRWLockShared" => sync::release_srw_shared as usize,
        "InitializeCriticalSection" => sync::initialize_critical_section as usize,
        "InitializeCriticalSectionAndSpinCount" => sync::initialize_critical_section_and_spin_count as usize,
        "InitializeCriticalSectionEx" => sync::initialize_critical_section_ex as usize,
        "EnterCriticalSection" => sync::enter_critical_section as usize,
        "TryEnterCriticalSection" => sync::try_enter_critical_section as usize,
        "LeaveCriticalSection" => sync::leave_critical_section as usize,
        "DeleteCriticalSection" => sync::delete_critical_section as usize,
        "SleepConditionVariableSRW" => sync::sleep_condition_variable_srw as usize,
        "WakeConditionVariable" => sync::wake_condition_variable as usize,
        "WakeAllConditionVariable" => sync::wake_all_condition_variable as usize,
        "InitOnceExecuteOnce" => sync::init_once_execute_once as usize,

        // oggetti kernel (eventi, mutex, thread, attese)
        "CreateEventW" => handles::create_event_w as usize,
        "OpenEventW" => handles::open_event_w as usize,
        "SetEvent" => handles::set_event as usize,
        "ResetEvent" => handles::reset_event as usize,
        "PulseEvent" => handles::pulse_event as usize,
        "CreateMutexW" => handles::create_mutex_w as usize,
        "OpenMutexW" => handles::open_mutex_w as usize,
        "ReleaseMutex" => handles::release_mutex as usize,
        "WaitForSingleObject" => handles::wait_for_single_object as usize,
        "WaitForSingleObjectEx" => handles::wait_for_single_object_ex as usize,
        "WaitForMultipleObjects" => handles::wait_for_multiple_objects as usize,
        "CloseHandle" => handles::close_handle as usize,
        "CreateThread" => handles::create_thread as usize,
        "ExitThread" => handles::exit_thread as usize,
        "GetExitCodeThread" => handles::get_exit_code_thread as usize,
        "FreeLibraryAndExitThread" => handles::free_library_and_exit_thread as usize,

        // TLS e FLS
        "TlsAlloc" => tls_alloc as usize,
        "TlsFree" => tls_free as usize,
        "TlsGetValue" => tls_get_value as usize,
        "TlsSetValue" => tls_set_value as usize,
        "FlsAlloc" => fls_alloc as usize,
        "FlsFree" => fls_free as usize,
        "FlsGetValue" => fls_get_value as usize,
        "FlsSetValue" => fls_set_value as usize,

        // WinRT / stringhe HSTRING
        "WindowsCreateString" => winrt::windows_create_string as usize,
        "WindowsCreateStringReference" => winrt::windows_create_string_reference as usize,
        "WindowsGetStringRawBuffer" => winrt::windows_get_string_raw_buffer as usize,
        "WindowsIsStringEmpty" => winrt::windows_is_string_empty as usize,
        "WindowsStringHasEmbeddedNull" => winrt::windows_string_has_embedded_null as usize,
        "RoOriginateError" => winrt::ro_originate_error as usize,
        "RoOriginateErrorW" => winrt::ro_originate_error_w as usize,

        // COM
        "CoInitializeEx" => com::co_initialize_ex as usize,
        "CoUninitialize" => com::co_uninitialize as usize,
        "CoCreateGuid" => com::co_create_guid as usize,
        "StringFromGUID2" => com::string_from_guid2 as usize,
        "CoCreateFreeThreadedMarshaler" => com::co_create_free_threaded_marshaler as usize,
        "PropVariantClear" => com::prop_variant_clear as usize,
        "InitPropVariantFromCLSID" => com::init_prop_variant_from_clsid as usize,

        // loader e risorse
        "LoadLibraryExW" => libloader::load_library_ex_w as usize,
        "LoadLibraryExA" => libloader::load_library_ex_a as usize,
        "LoadLibraryW" => libloader::load_library_w as usize,
        "LoadLibraryA" => libloader::load_library_a as usize,
        "FreeLibrary" => libloader::free_library as usize,
        "GetModuleHandleW" => libloader::get_module_handle_w as usize,
        "GetModuleHandleExW" => libloader::get_module_handle_ex_w as usize,
        "GetModuleHandleExA" => libloader::get_module_handle_ex_a as usize,
        "GetModuleFileNameW" => libloader::get_module_file_name_w as usize,
        "GetProcAddress" => libloader::get_proc_address as usize,
        "DisableThreadLibraryCalls" => libloader::disable_thread_library_calls as usize,
        "FindResourceW" => libloader::find_resource_w as usize,
        "LoadResource" => libloader::load_resource as usize,
        "LockResource" => libloader::lock_resource as usize,
        "SizeofResource" => libloader::size_of_resource as usize,

        // memoria condivisa (parametri real-time DSO) e protezione pagine
        "CreateFileMappingW" => mm::create_file_mapping_w as usize,
        "OpenFileMappingW" => mm::open_file_mapping_w as usize,
        "MapViewOfFile" => mm::map_view_of_file as usize,
        "UnmapViewOfFile" => mm::unmap_view_of_file as usize,
        "VirtualProtect" => mm::virtual_protect as usize,

        // ETW (log interno del modulo)
        "EventRegister" => etw::event_register as usize,
        "EventUnregister" => etw::event_unregister as usize,
        "EventWriteString" => etw::event_write_string as usize,

        // AVRT e RTWorkQ
        "AvSetMmThreadCharacteristicsW" => crt::av_set_mm_thread_characteristics_w as usize,
        "AvRevertMmThreadCharacteristics" => crt::av_revert_mm_thread_characteristics as usize,
        "RtwqPutWorkItem" => crt::rtwq_put_work_item as usize,
        "RtwqCreateAsyncResult" => crt::rtwq_create_async_result as usize,

        // il resto del CRT statico
        "GetLastError" => crt::get_last_error as usize,
        "SetLastError" => crt::set_last_error as usize,
        "RaiseException" => crt::raise_exception as usize,
        "RtlVirtualUnwind" => crt::rtl_virtual_unwind as usize,
        "RtlLookupFunctionEntry" => crt::rtl_lookup_function_entry as usize,
        "RtlCaptureContext" => crt::rtl_capture_context as usize,
        "RtlUnwindEx" => crt::rtl_unwind_ex as usize,
        "RtlPcToFileHeader" => crt::rtl_pc_to_file_header as usize,
        "TerminateProcess" => crt::terminate_process as usize,
        "ExitProcess" => crt::exit_process as usize,
        "SetUnhandledExceptionFilter" => crt::set_unhandled_exception_filter as usize,
        "UnhandledExceptionFilter" => crt::unhandled_exception_filter as usize,
        "QueryPerformanceCounter" => crt::query_performance_counter as usize,
        "QueryPerformanceFrequency" => crt::query_performance_frequency as usize,
        "GetSystemTimeAsFileTime" => crt::get_system_time_as_file_time as usize,
        "GetTickCount64" => crt::get_tick_count_64 as usize,
        "GetCurrentThreadId" => crt::get_current_thread_id as usize,
        "GetCurrentProcessId" => crt::get_current_process_id as usize,
        "GetCurrentProcess" => crt::get_current_process as usize,
        "IsDebuggerPresent" => crt::is_debugger_present as usize,
        "IsProcessorFeaturePresent" => crt::is_processor_feature_present as usize,
        "OutputDebugStringW" => crt::output_debug_string_w as usize,
        "InitializeSListHead" => crt::initialize_slist_head as usize,
        "InterlockedFlushSList" => crt::interlocked_flush_slist as usize,
        "GetStartupInfoW" => crt::get_startup_info_w as usize,
        "DecodePointer" => crt::decode_pointer as usize,
        "EncodePointer" => crt::encode_pointer as usize,
        "CompareStringOrdinal" => crt::compare_string_ordinal as usize,
        "GetCPInfo" => crt::get_cp_info as usize,
        "GetStringTypeW" => crt::get_string_type_w as usize,
        "MultiByteToWideChar" => crt::multi_byte_to_wide_char as usize,
        "WideCharToMultiByte" => crt::wide_char_to_multi_byte as usize,
        "IsValidCodePage" => crt::is_valid_code_page as usize,
        "GetACP" => crt::get_acp as usize,
        "GetOEMCP" => crt::get_oem_cp as usize,
        "GetThreadLocale" => crt::get_thread_locale as usize,
        "LCMapStringW" => crt::lc_map_string_w as usize,
        "FormatMessageW" => crt::format_message_w as usize,
        "FormatMessageA" => crt::format_message_a as usize,
        "GetEnvironmentVariableA" => crt::get_environment_variable_a as usize,
        "GetEnvironmentStringsW" => crt::get_environment_strings_w as usize,
        "FreeEnvironmentStringsW" => crt::free_environment_strings_w as usize,
        "GetCommandLineA" => crt::get_command_line_a as usize,
        "GetCommandLineW" => crt::get_command_line_w as usize,
        "GetStdHandle" => crt::get_std_handle as usize,
        "SetStdHandle" => crt::set_std_handle as usize,
        "GetConsoleMode" => crt::get_console_mode as usize,
        "GetConsoleOutputCP" => crt::get_console_output_cp as usize,
        "WriteConsoleW" => crt::write_console_w as usize,
        "CreateFileW" => crt::create_file_w as usize,
        "FindFirstFileExW" => crt::find_first_file_ex_w as usize,
        "FindNextFileW" => crt::find_next_file_w as usize,
        "FindClose" => crt::find_close as usize,
        "GetFileSizeEx" => crt::get_file_size_ex as usize,
        "GetFileType" => crt::get_file_type as usize,
        "FlushFileBuffers" => crt::flush_file_buffers as usize,
        "WriteFile" => crt::write_file as usize,
        "SetFilePointerEx" => crt::set_file_pointer_ex as usize,
        "ConvertStringSecurityDescriptorToSecurityDescriptorW" => {
            crt::convert_sddl_w as usize
        }

        // MessageBox: il CRT statico risolve gli assert via GetProcAddress(user32).
        "MessageBoxW" => crt::message_box_w as usize,
        "MessageBoxA" => crt::message_box_a as usize,

        _ => return None,
    })
}

// ---- TLS / FLS del sistema (TlsAlloc e compagni) ----

static FLS_CALLBACKS: std::sync::LazyLock<std::sync::Mutex<Vec<Option<usize>>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

pub fn fls_callback(idx: u32) -> Option<usize> {
    FLS_CALLBACKS.lock().unwrap().get(idx as usize).copied().flatten()
}

pub(crate) unsafe extern "win64" fn tls_alloc() -> u32 {
    match crate::teb::sys_tls_alloc() {
        Some(i) => i,
        None => 0xFFFF_FFFF,
    }
}

pub(crate) unsafe extern "win64" fn tls_free(idx: u32) -> i32 {
    crate::teb::sys_tls_free(idx) as i32
}

pub(crate) unsafe extern "win64" fn tls_get_value(idx: u32) -> *mut std::ffi::c_void {
    crate::teb::set_last_error(0);
    crate::teb::sys_tls_get(idx).map(|v| v as *mut std::ffi::c_void).unwrap_or(std::ptr::null_mut())
}

pub(crate) unsafe extern "win64" fn tls_set_value(idx: u32, value: *mut std::ffi::c_void) -> i32 {
    crate::teb::sys_tls_set(idx, if value.is_null() { None } else { Some(value as usize) });
    1
}

type FlsCallback = unsafe extern "win64" fn(*mut std::ffi::c_void);

pub(crate) unsafe extern "win64" fn fls_alloc(cb: Option<FlsCallback>) -> u32 {
    let idx = crate::teb::sys_tls_alloc().unwrap_or(u32::MAX);
    if idx != u32::MAX {
        let mut cbs = FLS_CALLBACKS.lock().unwrap();
        if cbs.len() <= idx as usize {
            cbs.resize(idx as usize + 1, None);
        }
        cbs[idx as usize] = cb.map(|f| f as usize);
    }
    idx
}

pub(crate) unsafe extern "win64" fn fls_free(idx: u32) -> i32 {
    let mut cbs = FLS_CALLBACKS.lock().unwrap();
    if let Some(slot) = cbs.get_mut(idx as usize) {
        *slot = None;
    }
    crate::teb::sys_tls_free(idx) as i32
}

pub(crate) unsafe extern "win64" fn fls_get_value(idx: u32) -> *mut std::ffi::c_void {
    crate::teb::fls_get(idx).map(|v| v as *mut std::ffi::c_void).unwrap_or(std::ptr::null_mut())
}

pub(crate) unsafe extern "win64" fn fls_set_value(idx: u32, value: *mut std::ffi::c_void) -> i32 {
    crate::teb::fls_set(idx, if value.is_null() { None } else { Some(value as usize) });
    1
}
