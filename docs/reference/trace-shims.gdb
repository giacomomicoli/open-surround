# trace-shims.gdb — quali shim "a rischio" di os-pe-loader vengono chiamati davvero dal modulo
# durante un'elaborazione offline (doc sez. 12.12, fase 3-bis). Stampa una riga per chiamata,
# con gli argomenti utili, il valore restituito dove conta e (per alcuni) l'RVA del chiamante
# nel modulo (base preferita 0x180000000, ottenuta dal loader).
#
# Uso (da build debug):
#   gdb -q -batch -x docs/reference/trace-shims.gdb --args target/debug/os-dap-linux \
#       --capture target/verify/gaming-performance/stereo \
#       --dll ~/.local/share/open-surround/DolbyAudioProcessing.dll --volume-db -9.3428 \
#       target/verify/signals/stereo.wav /tmp/out.wav 2>&1 | grep '^\[trace\]' | sort | uniq -c
#
# Risultati e confronto con la prima esecuzione: doc sez. 12.12, tabella in testa.

set pagination off
set breakpoint pending on
set print thread-events off

python
import gdb

MODULE_BASE = 0x180000000

def wide(expr):
    """Stringa UTF-16 terminata da NUL all'indirizzo `expr` (o '-' se nullo)."""
    p = int(gdb.parse_and_eval(expr))
    if p == 0:
        return "-"
    inf = gdb.selected_inferior()
    out = bytearray()
    while len(out) < 1024:
        b = bytes(inf.read_memory(p + len(out), 2))
        if b == b"\0\0":
            break
        out += b
    return out.decode("utf-16-le", "replace")

def caller_rva():
    f = gdb.newest_frame().older()
    if f is None:
        return "?"
    pc = f.pc()
    return "rva %#x" % (pc - MODULE_BASE) if MODULE_BASE <= pc < MODULE_BASE + 0x435000 else "pc %#x" % pc

class Ret(gdb.FinishBreakpoint):
    """Stampa rax al ritorno dello shim (e, se `out` è dato, il valore scritto lì)."""
    def __init__(self, label, out=None):
        super().__init__(gdb.newest_frame(), internal=True)
        self.silent = True
        self.label = label
        self.out = out
    def stop(self):
        rax = int(gdb.parse_and_eval("$rax")) & 0xFFFFFFFFFFFFFFFF
        extra = ""
        if self.out is not None and self.out != 0:
            v = int.from_bytes(bytes(gdb.selected_inferior().read_memory(self.out, 4)), "little")
            extra = " out=%#x" % v
        print("[trace] %s -> %#x%s" % (self.label, rax, extra))
        return False
    def out_of_scope(self):
        pass

class Trace(gdb.Breakpoint):
    def __init__(self, spec, describe, ret=None):
        super().__init__(spec, internal=False)
        self.silent = True
        self.describe = describe
        self.ret = ret
    def stop(self):
        try:
            label = self.describe()
        except gdb.error as e:
            label = "%s (%s)" % (self.location, e)
        print("[trace] " + label)
        if self.ret is not None:
            try:
                Ret(*self.ret(label))
            except gdb.error:
                pass  # argomento assente (build più vecchia): niente valore di ritorno
        return False

def ev(e):
    return int(gdb.parse_and_eval(e))

S = "os_pe_loader::shims::"
Trace(S + "mm::virtual_protect",
      lambda: "VirtualProtect addr=%#x size=%#x new=%#x old_ptr_null=%d"
              % (ev("addr"), ev("size"), ev("new_protect"), ev("old") == 0),
      ret=lambda l: ("VirtualProtect", ev("old")))
Trace(S + "mm::create_file_mapping_w",
      lambda: "CreateFileMappingW size=%#x name=%s" % (ev("size_low"), wide("name")),
      ret=lambda l: ("CreateFileMappingW",))
Trace(S + "handles::create_event_w",
      lambda: "CreateEventW manual=%d initial=%d name=%s" % (ev("manual"), ev("initial"), wide("name")))
Trace(S + "handles::create_mutex_w",
      lambda: "CreateMutexW initial=%d name=%s" % (ev("initial"), wide("name")))
Trace(S + "handles::open_event_w", lambda: "OpenEventW name=%s" % wide("name"))
Trace(S + "handles::open_mutex_w", lambda: "OpenMutexW name=%s" % wide("name"))
Trace(S + "mm::open_file_mapping_w", lambda: "OpenFileMappingW name=%s" % wide("name"))
Trace(S + "handles::close_handle", lambda: "CloseHandle")
Trace(S + "handles::create_thread", lambda: "CreateThread start=%#x" % ev("start"))
Trace(S + "handles::thread_finished", lambda: "thread terminato code=%u" % ev("code"))
Trace(S + "handles::exit_thread", lambda: "ExitThread")
Trace(S + "handles::pulse_event", lambda: "PulseEvent")
Trace(S + "handles::wait_for_multiple_objects", lambda: "WaitForMultipleObjects")
Trace(S + "sync::sleep_condition_variable_srw", lambda: "SleepConditionVariableSRW timeout=%u" % ev("timeout_ms"))
Trace(S + "sync::init_once_execute_once",
      lambda: "InitOnceExecuteOnce context_null=%d" % (ev("context") == 0),
      ret=lambda l: ("InitOnceExecuteOnce",))
Trace(S + "rtwq::rtwq_put_work_item", lambda: "RtwqPutWorkItem")
Trace(S + "rtwq::rtwq_create_async_result", lambda: "RtwqCreateAsyncResult")
Trace(S + "crt::is_processor_feature_present", lambda: "IsProcessorFeaturePresent %u" % ev("feature"))
Trace(S + "winrt::windows_string_has_embedded_null",
      lambda: "WindowsStringHasEmbeddedNull da %s" % caller_rva(),
      ret=lambda l: ("WindowsStringHasEmbeddedNull", ev("has_embed_null")))
Trace(S + "libloader::get_module_file_name_w",
      lambda: "GetModuleFileNameW hmod=%#x size=%u" % (ev("hmod"), ev("size")),
      ret=lambda l: ("GetModuleFileNameW size=%u" % ev("size"),))
Trace(S + "libloader::get_proc_address",
      lambda: "GetProcAddress hmod=%#x %s" % (ev("hmod"), gdb.parse_and_eval("name").string()),
      ret=lambda l: (l.replace("[trace] ", ""),))
Trace(S + "libloader::disable_thread_library_calls", lambda: "DisableThreadLibraryCalls hmod=%#x" % ev("hmod"))
Trace("os_pe_loader::loader::Image::dll_main", lambda: "DllMain reason=%u" % ev("reason"))
end

run
quit
