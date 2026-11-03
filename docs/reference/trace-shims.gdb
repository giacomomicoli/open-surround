# trace-shims.gdb — quali shim "a rischio" di os-pe-loader vengono chiamati davvero dal modulo
# durante un'elaborazione offline (doc sez. 12.12, fase 3-bis). Stampa una riga per chiamata.
#
# Uso (da build debug):
#   gdb -q -batch -x docs/reference/trace-shims.gdb --args target/debug/os-dap-linux \
#       --capture target/verify/gaming-performance/stereo \
#       --dll ~/Documenti/dolby-bins/DolbyAudioProcessing.dll --volume-db -9.3428 \
#       target/verify/signals/stereo.wav /tmp/out.wav 2>&1 | grep '^\[trace\]' | sort | uniq -c
#
# Risultato del 3/11/2026 (stereo, Gaming performance): VirtualProtect ×9 (sempre .fptable,
# 0x18042c000, allineato, alterna PAGE_READONLY/PAGE_READWRITE), CreateEventW ×4 (2 con nome),
# CreateMutexW ×2, CreateFileMappingW ×2-3, CreateThread ×1 (il thread non termina prima
# dell'uscita del processo), InitOnceExecuteOnce ×3, GetModuleFileNameW ×2,
# WindowsStringHasEmbeddedNull ×1 (durante l'attivazione: percorso vivo!), GetProcAddress per
# 10 funzioni opzionali del CRT (tutte NULL → ripiego del CRT); nessuna chiamata a RTWorkQ,
# SleepConditionVariableSRW, WaitForMultipleObjects, PulseEvent, ExitThread, CloseHandle,
# IsProcessorFeaturePresent.

set pagination off
set breakpoint pending on

break os_pe_loader::shims::mm::virtual_protect
commands
silent
printf "[trace] VirtualProtect addr=%p size=%#lx new=%#x\n", addr, size, new_protect
continue
end

break os_pe_loader::shims::mm::create_file_mapping_w
commands
silent
printf "[trace] CreateFileMappingW size_low=%#x name=%p\n", size_low, name
continue
end

break os_pe_loader::shims::handles::create_event_w
commands
silent
printf "[trace] CreateEventW manual=%d initial=%d name=%p\n", manual, initial, name
continue
end

break os_pe_loader::shims::handles::create_mutex_w
commands
silent
printf "[trace] CreateMutexW\n"
continue
end

break os_pe_loader::shims::handles::close_handle
commands
silent
printf "[trace] CloseHandle h=%#lx\n", h
continue
end

break os_pe_loader::shims::handles::create_thread
commands
silent
printf "[trace] CreateThread start=%p\n", start
continue
end

break os_pe_loader::shims::handles::thread_finished
commands
silent
printf "[trace] thread terminato code=%u\n", code
continue
end

break os_pe_loader::shims::handles::exit_thread
commands
silent
printf "[trace] ExitThread\n"
continue
end

break os_pe_loader::shims::handles::pulse_event
commands
silent
printf "[trace] PulseEvent\n"
continue
end

break os_pe_loader::shims::handles::wait_for_multiple_objects
commands
silent
printf "[trace] WaitForMultipleObjects\n"
continue
end

break os_pe_loader::shims::sync::sleep_condition_variable_srw
commands
silent
printf "[trace] SleepConditionVariableSRW timeout=%u\n", timeout_ms
continue
end

break os_pe_loader::shims::sync::init_once_execute_once
commands
silent
printf "[trace] InitOnceExecuteOnce\n"
continue
end

break os_pe_loader::shims::crt::rtwq_put_work_item
commands
silent
printf "[trace] RtwqPutWorkItem\n"
continue
end

break os_pe_loader::shims::crt::rtwq_create_async_result
commands
silent
printf "[trace] RtwqCreateAsyncResult\n"
continue
end

break os_pe_loader::shims::crt::is_processor_feature_present
commands
silent
printf "[trace] IsProcessorFeaturePresent %u\n", feature
continue
end

break os_pe_loader::shims::winrt::windows_string_has_embedded_null
commands
silent
printf "[trace] WindowsStringHasEmbeddedNull\n"
continue
end

break os_pe_loader::shims::libloader::get_module_file_name_w
commands
silent
printf "[trace] GetModuleFileNameW size=%u\n", size
continue
end

break os_pe_loader::shims::libloader::get_proc_address
commands
silent
printf "[trace] GetProcAddress %s\n", name
continue
end

run
quit
