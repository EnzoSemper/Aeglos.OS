pub mod page;
pub mod vmm;

pub use page::{PAGE_SIZE, init, alloc_page, alloc_pages, free_page, release_pages, free_pages, used_pages, total_pages};

// --- LibC / LibM Exports for C++ ---

#[no_mangle]
pub extern "C" fn powf(base: f32, exp: f32) -> f32 { libm::powf(base, exp) }

#[no_mangle]
pub extern "C" fn expf(n: f32) -> f32 { libm::expf(n) }

#[no_mangle]
pub extern "C" fn logf(n: f32) -> f32 { libm::logf(n) }

#[no_mangle]
pub extern "C" fn sqrtf(n: f32) -> f32 { libm::sqrtf(n) }

#[no_mangle]
pub extern "C" fn sinf(n: f32) -> f32 { libm::sinf(n) }

#[no_mangle]
pub extern "C" fn cosf(n: f32) -> f32 { libm::cosf(n) }

#[no_mangle]
pub extern "C" fn tanf(n: f32) -> f32 { libm::tanf(n) }

#[no_mangle]
pub extern "C" fn acosf(n: f32) -> f32 { libm::acosf(n) }

#[no_mangle]
pub extern "C" fn asinf(n: f32) -> f32 { libm::asinf(n) }

#[no_mangle]
pub extern "C" fn atanf(n: f32) -> f32 { libm::atanf(n) }

#[no_mangle]
pub extern "C" fn fminf(a: f32, b: f32) -> f32 { libm::fminf(a, b) }

#[no_mangle]
pub extern "C" fn fmaxf(a: f32, b: f32) -> f32 { libm::fmaxf(a, b) }

#[no_mangle]
pub extern "C" fn roundf(n: f32) -> f32 { libm::roundf(n) }

#[no_mangle]
pub extern "C" fn floorf(n: f32) -> f32 { libm::floorf(n) }

#[no_mangle]
pub extern "C" fn ceilf(n: f32) -> f32 { libm::ceilf(n) }

#[no_mangle]
pub extern "C" fn fmodf(a: f32, b: f32) -> f32 { libm::fmodf(a, b) }

#[no_mangle]
pub extern "C" fn tanhf(n: f32) -> f32 { libm::tanhf(n) }

#[no_mangle]
pub extern "C" fn erff(n: f32) -> f32 { libm::erff(n) }

// Double precision (if needed) usually llama.cpp uses float (f32) for most things, 
// but some quantization might use double.
#[no_mangle]
pub extern "C" fn pow(n: f64, e: f64) -> f64 { libm::pow(n, e) }

#[no_mangle]
pub extern "C" fn sqrt(n: f64) -> f64 { libm::sqrt(n) }

// --- Memory Allocator Exports (Moved/Verified) ---
// (Already present: malloc, free, calloc, realloc)

pub mod heap;

/*
#[no_mangle]
pub unsafe extern "C" fn malloc(size: usize) -> *mut u8 {
    let uart = crate::drivers::uart::Uart::new();
    uart.puts("[mem] malloc ");
    uart.put_dec(size);
    uart.puts("\r\n");
    heap::c_malloc(size)
}

#[no_mangle]
pub unsafe extern "C" fn free(ptr: *mut u8) {
    heap::c_free(ptr)
}

#[no_mangle]
pub unsafe extern "C" fn calloc(nmemb: usize, size: usize) -> *mut u8 {
    let total_size = nmemb * size;
    let ptr = malloc(total_size);
    if !ptr.is_null() {
        core::ptr::write_bytes(ptr, 0, total_size);
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn realloc(ptr: *mut u8, size: usize) -> *mut u8 {
    heap::c_realloc(ptr, size)
}

#[no_mangle]
pub unsafe extern "C" fn aligned_alloc(alignment: usize, size: usize) -> *mut u8 {
    heap::c_aligned_alloc(size, alignment)
}
*/
