//! Heap: HeapAlloc/CoTaskMemAlloc/LocalAlloc con un unico allocatore Rust a 16 byte di
//! allineamento, con header che ricorda la dimensione (HeapSize/HeapReAlloc la richiedono).
//! Il modulo alloca con uno di questi e libera con l'altro: un solo allocatore li copre.

use std::alloc::{alloc, dealloc, Layout};

/// Header a 16 byte: dimensione richiesta + magico per accorgersi di free stranieri.
const HDR: usize = 16;
const MAGIC: u64 = 0x7769_6E68_6561_7031; // "winheap1"

unsafe fn inner_alloc(size: usize, zero: bool) -> *mut u8 {
    let size = size.max(1);
    let layout = Layout::from_size_align(size + HDR, 16).unwrap();
    let p = if zero { std::alloc::alloc_zeroed(layout) } else { unsafe { alloc(layout) } };
    if p.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        *(p as *mut u64) = size as u64;
        *(p.add(8) as *mut u64) = MAGIC;
    }
    unsafe { p.add(HDR) }
}

unsafe fn inner_free(p: *mut u8) {
    if p.is_null() {
        return;
    }
    let p = p.sub(HDR);
    unsafe {
        let magic = *(p.add(8) as *const u64);
        if magic != MAGIC {
            eprintln!("[os-pe-loader] free di un puntatore non nostro: {p:?} (ignorato)");
            return;
        }
        let size = *(p as *const u64) as usize;
        dealloc(p, Layout::from_size_align(size + HDR, 16).unwrap());
    }
}

fn inner_size(p: *mut u8) -> usize {
    if p.is_null() {
        return 0;
    }
    unsafe { *(p.sub(HDR) as *const u64) as usize }
}

unsafe fn inner_realloc(p: *mut u8, new_size: usize, zero_tail: bool) -> *mut u8 {
    if p.is_null() {
        return inner_alloc(new_size, false);
    }
    let old = inner_size(p);
    let q = inner_alloc(new_size, false);
    if q.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let copy = old.min(new_size);
        std::ptr::copy_nonoverlapping(p as *const u8, q, copy);
        if zero_tail && new_size > old {
            std::ptr::write_bytes(q.add(old), 0, new_size - old);
        }
        inner_free(p);
    }
    q
}

/// TRUE = 1 per HeapFree/LocalFree.
pub const HEAP_ZERO_MEMORY: u32 = 0x8;

pub(crate) unsafe extern "win64" fn get_process_heap() -> *mut std::ffi::c_void {
    1usize as *mut std::ffi::c_void
}

pub(crate) unsafe extern "win64" fn heap_alloc(_heap: *mut std::ffi::c_void, flags: u32, size: usize) -> *mut std::ffi::c_void {
    let p = inner_alloc(size, flags & HEAP_ZERO_MEMORY != 0);
    if p.is_null() {
        crate::teb::set_last_error(8 /* ERROR_NOT_ENOUGH_MEMORY */);
    }
    p as *mut std::ffi::c_void
}

pub(crate) unsafe extern "win64" fn heap_free(_heap: *mut std::ffi::c_void, _flags: u32, ptr: *mut std::ffi::c_void) -> i32 {
    unsafe { inner_free(ptr as *mut u8) };
    1
}

pub(crate) unsafe extern "win64" fn heap_realloc(
    _heap: *mut std::ffi::c_void,
    flags: u32,
    ptr: *mut std::ffi::c_void,
    size: usize,
) -> *mut std::ffi::c_void {
    inner_realloc(ptr as *mut u8, size, flags & HEAP_ZERO_MEMORY != 0) as *mut std::ffi::c_void
}

pub(crate) unsafe extern "win64" fn heap_size(_heap: *mut std::ffi::c_void, _flags: u32, ptr: *mut std::ffi::c_void) -> usize {
    inner_size(ptr as *mut u8)
}

pub(crate) unsafe extern "win64" fn co_task_mem_alloc(size: usize) -> *mut std::ffi::c_void {
    inner_alloc(size, false) as *mut std::ffi::c_void
}

pub(crate) unsafe extern "win64" fn co_task_mem_free(ptr: *mut std::ffi::c_void) {
    unsafe { inner_free(ptr as *mut u8) };
}

pub(crate) unsafe fn alloc_bytes(size: usize, zero: bool) -> *mut u8 {
    inner_alloc(size, zero)
}

/// Per il backend Linux: allocazioni che il modulo può liberare con CoTaskMemFree/LocalFree
/// (il suo heap è lo stesso).
pub fn allocate(size: usize, zero: bool) -> *mut u8 {
    unsafe { inner_alloc(size, zero) }
}

pub(crate) unsafe fn free_bytes(ptr: *mut u8) {
    inner_free(ptr)
}

pub(crate) unsafe extern "win64" fn local_alloc(flags: u32, size: usize) -> *mut std::ffi::c_void {
    // LMEM_ZEROINIT = 0x40, LMEM_MOVEABLE ignorato (i locali del modulo li usa fissi).
    inner_alloc(size, flags & 0x40 != 0) as *mut std::ffi::c_void
}

pub(crate) unsafe extern "win64" fn local_free(ptr: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    unsafe { inner_free(ptr as *mut u8) };
    std::ptr::null_mut()
}
