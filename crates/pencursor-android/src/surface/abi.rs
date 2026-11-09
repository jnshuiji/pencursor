//! AOSP `SurfaceFlinger` / NDK `ANativeWindow` memory layouts and AArch64 AAPCS64 `sret` trampolines.

use std::ffi::c_void;
use std::ptr;

pub(super) const PIXEL_FORMAT_RGBA_8888: i32 = 1;
pub(super) const LAYER_FLAG_SKIP_SCREENSHOT: u32 = 0x40;
pub(super) const TOPMOST_Z_ORDER: i32 = 0x3FFF_FFFF;

const ANDROID_NATIVE_WINDOW_MAGIC: u32 = 0x5F77_6E64; // "_wnd"
const ANDROID_NATIVE_WINDOW_MAGIC_UPPER: u32 = 0x5F57_4E44; // "_WND"

/// NDK `ANativeWindow_Buffer` layout (`<android/native_window.h>`).
#[repr(C)]
#[derive(Debug)]
pub(super) struct ANativeWindowBuffer {
    pub(super) width: i32,
    pub(super) height: i32,
    pub(super) stride: i32,
    pub(super) format: i32,
    pub(super) bits: *mut c_void,
    pub(super) reserved: [u32; 6],
}

impl Default for ANativeWindowBuffer {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            stride: 0,
            format: 0,
            bits: ptr::null_mut(),
            reserved: [0; 6],
        }
    }
}

/// 16-byte aligned 2048-byte oversized buffer for `SurfaceComposerClient::Transaction`.
#[repr(C, align(16))]
#[derive(Debug)]
pub(super) struct TransactionSlab(pub(super) [u64; 256]);

impl Default for TransactionSlab {
    fn default() -> Self {
        Self([0u64; 256])
    }
}

/// 16-byte aligned 256-byte oversized buffer for `gui::LayerMetadata`.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct LayerMetadataSlab(pub(super) [u64; 32]);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CreateSurfaceVariant {
    BinderParent(usize),
    RawControlParent(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum TxApplyVariant {
    OneBool(usize),
    TwoBools(usize),
}

/// AArch64 AAPCS64 `x8` (`sret`) trampoline for single-argument member functions returning `android::sp<T>`.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub(super) unsafe fn call_sret_sp1(func: usize, this_x0: *mut c_void) -> *mut c_void {
    let mut out_sp: *mut c_void = ptr::null_mut();
    // SAFETY: Passes `&mut out_sp` in `x8` per AArch64 Itanium C++ ABI for non-trivial `sp<T>` return.
    unsafe {
        core::arch::asm!(
            "blr {func}",
            func = in(reg) func,
            in("x8") &raw mut out_sp,
            inout("x0") this_x0 => _,
            clobber_abi("C"),
        );
    }
    out_sp
}

#[cfg(not(target_arch = "aarch64"))]
#[inline(always)]
pub(super) unsafe fn call_sret_sp1(_func: usize, _this_x0: *mut c_void) -> *mut c_void {
    ptr::null_mut()
}

/// AArch64 AAPCS64 `x8` (`sret`) trampoline for `SurfaceComposerClient::createSurface`.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn call_sret_create_surface(
    func: usize,
    this_x0: *mut c_void,
    name_x1: *const c_void,
    width_w2: u32,
    height_w3: u32,
    format_w4: i32,
    flags_w5: u32,
    parent_x6: *const c_void,
    meta_x7: *mut c_void,
    out_hint_sp0: *mut u32,
) -> *mut c_void {
    let mut out_sp: *mut c_void = ptr::null_mut();
    // SAFETY: Allocates 16-byte aligned stack slot for the 9th parameter (`outTransformHint`) and binds `x8` for `sp<SurfaceControl>`.
    unsafe {
        core::arch::asm!(
            "sub sp, sp, #16",
            "str {hint}, [sp]",
            "blr {func}",
            "add sp, sp, #16",
            func = in(reg) func,
            hint = in(reg) out_hint_sp0,
            in("x8") &raw mut out_sp,
            inout("x0") this_x0 => _,
            in("x1") name_x1,
            in("w2") width_w2,
            in("w3") height_w3,
            in("w4") format_w4,
            in("w5") flags_w5,
            in("x6") parent_x6,
            in("x7") meta_x7,
            clobber_abi("C"),
        );
    }
    out_sp
}

#[cfg(not(target_arch = "aarch64"))]
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn call_sret_create_surface(
    _func: usize,
    _this_x0: *mut c_void,
    _name_x1: *const c_void,
    _width_w2: u32,
    _height_w3: u32,
    _format_w4: i32,
    _flags_w5: u32,
    _parent_x6: *const c_void,
    _meta_x7: *mut c_void,
    _out_hint_sp0: *mut u32,
) -> *mut c_void {
    ptr::null_mut()
}

/// Scans the `android::Surface` object header for the `_WND` / `_wnd` magic to locate the embedded `ANativeWindow*`.
pub(super) unsafe fn extract_native_window(surface_ptr: *mut c_void) -> Option<*mut c_void> {
    if surface_ptr.is_null() {
        return None;
    }
    let base = surface_ptr.cast::<u8>();
    for off in (0..128).step_by(8) {
        // SAFETY: `android::Surface` is >128 bytes and 8-byte aligned.
        unsafe {
            let ptr_at_off = base.add(off);
            let magic = *ptr_at_off.cast::<u32>();
            if magic == ANDROID_NATIVE_WINDOW_MAGIC || magic == ANDROID_NATIVE_WINDOW_MAGIC_UPPER {
                return Some(ptr_at_off.cast::<c_void>());
            }
        }
    }
    None
}
