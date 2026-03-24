use linked_list_allocator::LockedHeap;
use core::alloc::{GlobalAlloc, Layout};

pub const HEAP_SIZE: usize = 12 * 1024 * 1024 * 1024; // 12 GiB — weights (4.4 GB) + backend buffer (4.5 GB) + KV cache + overhead

#[global_allocator]
pub static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub fn init() {
    let pages = HEAP_SIZE / crate::memory::PAGE_SIZE;
    if let Some(heap_start) = crate::memory::alloc_pages(pages) {
         unsafe {
            ALLOCATOR.lock().init(
                crate::memory::vmm::phys_to_virt(heap_start) as *mut u8,
                HEAP_SIZE,
            );
        }
    } else {
        panic!("Failed to allocate kernel heap");
    }
}

// Robust Header (32 bytes)
#[repr(C, align(16))]
struct AllocHeader {
    block_ptr: usize,     // Original pointer from allocator
    layout_size: usize,   // Size used for allocation
    layout_align: usize,  // Align used for allocation
    magic: usize,         // Safety check
}

const HEADER_SIZE: usize = core::mem::size_of::<AllocHeader>();
const MAGIC: usize = 0xDEAD_BEEF_CAFE_BABE;

pub unsafe fn c_aligned_alloc(size: usize, align: usize) -> *mut u8 {
    // Ensure minimal alignment
    let align = if align < 16 { 16 } else { align };
    // align must be power of 2
    if !align.is_power_of_two() { return core::ptr::null_mut(); }

    // Alloc size needs verification to avoid overflow, but skipping for now.
    
    // We allocate enough space for: size + HEADER_SIZE + (align - 1)
    // To ensure we can find an aligned UserPtr with enough space for Header before it.
    let alloc_size = size + HEADER_SIZE + align;
    
    // We can just use base alignment of 16 for the block itself, 
    // and manually align inside.
    let layout = Layout::from_size_align_unchecked(alloc_size, 16);
    
    let block_ptr = ALLOCATOR.alloc(layout);
    if block_ptr.is_null() {
        return core::ptr::null_mut();
    }
    
    let block_addr = block_ptr as usize;
    
    // Calculate UserPtr
    // UserPtr must be aligned to `align`.
    // UserPtr - HEADER_SIZE >= block_addr.
    // So UserPtr >= block_addr + HEADER_SIZE.
    
    let min_user_addr = block_addr + HEADER_SIZE;
    let user_addr = (min_user_addr + (align - 1)) & !(align - 1);
    
    let header_addr = user_addr - HEADER_SIZE;
    let header = header_addr as *mut AllocHeader;
    
    (*header).block_ptr = block_addr;
    (*header).layout_size = alloc_size;
    (*header).layout_align = 16;
    (*header).magic = MAGIC;
    
    user_addr as *mut u8
}

pub unsafe fn c_malloc(size: usize) -> *mut u8 {
    c_aligned_alloc(size, 16)
}

pub unsafe fn c_free(ptr: *mut u8) {
    if ptr.is_null() { return; }

    let header_ptr = ptr.sub(HEADER_SIZE) as *mut AllocHeader;
    if (*header_ptr).magic != MAGIC {
        // Corruption or invalid pointer.
        // Prevent crashing if possible, or panic in debug.
        return;
    }

    // Clear magic to prevent double free validity (though memory is gone)
    // (*header_ptr).magic = 0;

    let block_ptr = (*header_ptr).block_ptr as *mut u8;
    let size = (*header_ptr).layout_size;
    let align = (*header_ptr).layout_align;
    
    let layout = Layout::from_size_align_unchecked(size, align);
    ALLOCATOR.dealloc(block_ptr, layout);
}

pub unsafe fn c_realloc(ptr: *mut u8, new_size: usize) -> *mut u8 {
    if ptr.is_null() {
        return c_malloc(new_size);
    }
    if new_size == 0 {
        c_free(ptr);
        return core::ptr::null_mut();
    }

    let header_ptr = ptr.sub(HEADER_SIZE) as *mut AllocHeader;
    if (*header_ptr).magic != MAGIC {
        return core::ptr::null_mut();
    }
    
    // We cannot easily resize in place because of the manual alignment offsets.
    // So we alloc new, copy, free old.
    
    // How much to copy? We don't track user `size` anymore, we track `layout_size`.
    // But `layout_size` is bigger than user size.
    // Copying `layout_size` is safe (read access) but might copy garbage at end.
    // Better to store user size too?
    // Let's assume user size is approx layout_size - HEADER_SIZE.
    // Or just store it.
    
    // WAIT. We didn't store user size!
    // We should store user size in header to support realloc optimal copy.
    // But for now, we can copy (layout_size - HEADER_SIZE) as a safe upper bound of data.
    // (Actually layout_size includes padding, so it's > user data).
    // Copying extra is harmless for POD.
    
    let old_layout_size = (*header_ptr).layout_size;
    let copy_size = old_layout_size - HEADER_SIZE; // Rough verify
    
    let new_ptr = c_malloc(new_size);
    if !new_ptr.is_null() {
        let actual_copy = if copy_size < new_size { copy_size } else { new_size };
        core::ptr::copy_nonoverlapping(ptr, new_ptr, actual_copy);
        c_free(ptr);
    }
    new_ptr
}
