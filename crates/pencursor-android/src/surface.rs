//! Native `SurfaceFlinger` dual-sprite overlay backend (`libgui.so` + `libandroid.so`).
//!
//! Architecture:
//! - Creates two `64x64` topmost click-through `SurfaceControl` sprite layers (`hover_layer`
//!   and `contact_layer`).
//! - Rasterizes and uploads the `Hover` and `Contact` vector sprites once at initialization
//!   using `tiny-skia` and `ANativeWindow_lock` / `ANativeWindow_unlockAndPost`.
//! - Performs zero runtime pixel writes or buffer locks: all cursor movement and
//!   hover/contact transitions are executed exclusively via asynchronous (`oneWay`)
//!   `SurfaceComposerClient::Transaction` (`setPosition`, `show`, `hide`).

use crate::elf::find_elf64_dynsym_names;
use crate::raster::{SPRITE_CENTER_OFFSET, SPRITE_SIZE, render_contact_sprite, render_hover_sprite};
use crate::sys::{DlLib, set_thread_euid};
use pencursor_core::{Config, OverlayBackend, StylusSnapshot, ViewportBounds};
use std::ffi::{CString, c_char, c_int, c_void};
use std::fmt;
use std::fs;
use std::ptr;
use tiny_skia::Pixmap;

const PIXEL_FORMAT_RGBA_8888: i32 = 1;
const LAYER_FLAG_SKIP_SCREENSHOT: u32 = 0x40;
const TOPMOST_Z_ORDER: i32 = 0x3FFF_FFFF;
const ANDROID_NATIVE_WINDOW_MAGIC: u32 = 0x5F77_6E64; // "_wnd"
const ANDROID_NATIVE_WINDOW_MAGIC_UPPER: u32 = 0x5F57_4E44; // "_WND"

/// Error type for `SurfaceFlinger` overlay operations.
#[derive(Debug, Clone)]
pub struct SurfaceError(pub String);

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SurfaceError {}

impl From<String> for SurfaceError {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// NDK `ANativeWindow_Buffer` layout (`<android/native_window.h>`).
#[repr(C)]
struct ANativeWindowBuffer {
    width: i32,
    height: i32,
    stride: i32,
    format: i32,
    bits: *mut c_void,
    reserved: [u32; 6],
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
struct TransactionSlab([u64; 256]);

impl Default for TransactionSlab {
    fn default() -> Self {
        Self([0u64; 256])
    }
}

/// 16-byte aligned 256-byte oversized buffer for `gui::LayerMetadata`.
#[repr(C, align(16))]
#[derive(Clone, Copy, Default)]
struct LayerMetadataSlab([u64; 32]);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CreateSurfaceVariant {
    BinderParent(usize),
    RawControlParent(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TxApplyVariant {
    OneBool(usize),
    TwoBools(usize),
}

/// Dynamically resolved symbol table from `libutils.so`, `libgui.so`, and `libandroid.so`.
#[derive(Debug)]
struct GuiSymbols {
    _libutils: DlLib,
    _libgui: DlLib,
    _libandroid: DlLib,

    string8_ctor: unsafe extern "C" fn(*mut c_void, *const c_char),
    string8_dtor: unsafe extern "C" fn(*mut c_void),
    refbase_inc: unsafe extern "C" fn(*mut c_void, *const c_void),
    refbase_dec: unsafe extern "C" fn(*mut c_void, *const c_void),

    scc_ctor: unsafe extern "C" fn(*mut c_void),
    scc_init_check: unsafe extern "C" fn(*const c_void) -> i32,
    scc_create_surface: CreateSurfaceVariant,
    sc_get_surface: usize,

    layer_meta_ctor: Option<unsafe extern "C" fn(*mut c_void)>,
    layer_meta_dtor: Option<unsafe extern "C" fn(*mut c_void)>,

    tx_ctor: unsafe extern "C" fn(*mut c_void),
    tx_dtor: Option<unsafe extern "C" fn(*mut c_void)>,
    tx_set_layer: unsafe extern "C" fn(*mut c_void, *const *mut c_void, i32) -> *mut c_void,
    tx_set_pos:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void, f32, f32) -> *mut c_void>,
    tx_set_flags:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void, u32, u32) -> *mut c_void>,
    tx_set_layer_stack:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void, u32) -> *mut c_void>,
    tx_set_trusted_overlay_bool:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void, bool) -> *mut c_void>,
    tx_set_trusted_overlay_enum:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void, i32) -> *mut c_void>,
    tx_set_drop_input_mode:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void, u32) -> *mut c_void>,
    tx_show: Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void) -> *mut c_void>,
    tx_hide: Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void) -> *mut c_void>,
    tx_apply: TxApplyVariant,

    anw_acquire: unsafe extern "C" fn(*mut c_void),
    anw_release: unsafe extern "C" fn(*mut c_void),
    anw_set_geometry: unsafe extern "C" fn(*mut c_void, i32, i32, i32) -> i32,
    anw_lock: unsafe extern "C" fn(*mut c_void, *mut ANativeWindowBuffer, *mut c_void) -> i32,
    anw_unlock_and_post: unsafe extern "C" fn(*mut c_void) -> i32,
}

impl GuiSymbols {
    fn resolve() -> Result<Self, String> {
        let libutils = DlLib::open("/system/lib64/libutils.so")
            .or_else(|_| DlLib::open("libutils.so"))?;
        let libgui =
            DlLib::open("/system/lib64/libgui.so").or_else(|_| DlLib::open("libgui.so"))?;
        let libandroid = DlLib::open("/system/lib64/libandroid.so")
            .or_else(|_| DlLib::open("libandroid.so"))?;

        // SAFETY: All transmuted function pointers match AOSP Itanium C++ ABI and NDK C ABI signatures.
        unsafe {
            let string8_ctor = core::mem::transmute::<
                usize,
                unsafe extern "C" fn(*mut c_void, *const c_char),
            >(
                libutils
                    .sym("_ZN7android7String8C1EPKc")
                    .or_else(|| libutils.sym("_ZN7android7String8C2EPKc"))
                    .ok_or("missing String8::String8(char const*)")?,
            );
            let string8_dtor =
                core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(
                    libutils
                        .sym("_ZN7android7String8D1Ev")
                        .or_else(|| libutils.sym("_ZN7android7String8D2Ev"))
                        .ok_or("missing String8::~String8()")?,
                );
            let refbase_inc = core::mem::transmute::<
                usize,
                unsafe extern "C" fn(*mut c_void, *const c_void),
            >(
                libutils
                    .sym("_ZNK7android7RefBase9incStrongEPKv")
                    .ok_or("missing RefBase::incStrong")?,
            );
            let refbase_dec = core::mem::transmute::<
                usize,
                unsafe extern "C" fn(*mut c_void, *const c_void),
            >(
                libutils
                    .sym("_ZNK7android7RefBase9decStrongEPKv")
                    .ok_or("missing RefBase::decStrong")?,
            );

            let scc_ctor = core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(
                libgui
                    .sym("_ZN7android21SurfaceComposerClientC1Ev")
                    .or_else(|| libgui.sym("_ZN7android21SurfaceComposerClientC2Ev"))
                    .ok_or("missing SurfaceComposerClient::SurfaceComposerClient()")?,
            );
            let scc_init_check =
                core::mem::transmute::<usize, unsafe extern "C" fn(*const c_void) -> i32>(
                    libgui
                        .sym("_ZNK7android21SurfaceComposerClient9initCheckEv")
                        .ok_or("missing SurfaceComposerClient::initCheck()")?,
                );

            let scc_create_surface = Self::resolve_create_surface(&libgui)?;

            let sc_get_surface = libgui
                .sym("_ZN7android14SurfaceControl10getSurfaceEv")
                .or_else(|| libgui.sym("_ZNK7android14SurfaceControl10getSurfaceEv"))
                .ok_or("missing SurfaceControl::getSurface()")?;

            let layer_meta_ctor = libgui
                .sym("_ZN7android3gui13LayerMetadataC1Ev")
                .or_else(|| libgui.sym("_ZN7android13LayerMetadataC1Ev"))
                .or_else(|| libgui.sym("_ZN7android3gui13LayerMetadataC2Ev"))
                .or_else(|| libgui.sym("_ZN7android13LayerMetadataC2Ev"))
                .map(|p| core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(p));

            let layer_meta_dtor = libgui
                .sym("_ZN7android3gui13LayerMetadataD1Ev")
                .or_else(|| libgui.sym("_ZN7android13LayerMetadataD1Ev"))
                .or_else(|| libgui.sym("_ZN7android3gui13LayerMetadataD2Ev"))
                .or_else(|| libgui.sym("_ZN7android13LayerMetadataD2Ev"))
                .map(|p| core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(p));

            let tx_ctor = core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(
                libgui
                    .sym("_ZN7android21SurfaceComposerClient11TransactionC1Ev")
                    .or_else(|| libgui.sym("_ZN7android21SurfaceComposerClient11TransactionC2Ev"))
                    .ok_or("missing Transaction::Transaction()")?,
            );
            let tx_dtor = libgui
                .sym("_ZN7android21SurfaceComposerClient11TransactionD1Ev")
                .or_else(|| libgui.sym("_ZN7android21SurfaceComposerClient11TransactionD2Ev"))
                .map(|p| core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(p));
            let tx_set_layer = core::mem::transmute::<
                usize,
                unsafe extern "C" fn(*mut c_void, *const *mut c_void, i32) -> *mut c_void,
            >(
                libgui
                    .sym("_ZN7android21SurfaceComposerClient11Transaction8setLayerERKNS_2spINS_14SurfaceControlEEEi")
                    .ok_or("missing Transaction::setLayer")?,
            );
            let tx_set_pos = libgui
                .sym("_ZN7android21SurfaceComposerClient11Transaction11setPositionERKNS_2spINS_14SurfaceControlEEEff")
                .map(|p| {
                    core::mem::transmute::<
                        usize,
                        unsafe extern "C" fn(*mut c_void, *const *mut c_void, f32, f32) -> *mut c_void,
                    >(p)
                });
            let tx_set_flags = libgui
                .sym("_ZN7android21SurfaceComposerClient11Transaction8setFlagsERKNS_2spINS_14SurfaceControlEEEjj")
                .map(|p| {
                    core::mem::transmute::<
                        usize,
                        unsafe extern "C" fn(*mut c_void, *const *mut c_void, u32, u32) -> *mut c_void,
                    >(p)
                });
            let tx_set_layer_stack = libgui
                .sym("_ZN7android21SurfaceComposerClient11Transaction13setLayerStackERKNS_2spINS_14SurfaceControlEEEj")
                .or_else(|| {
                    libgui.sym("_ZN7android21SurfaceComposerClient11Transaction13setLayerStackERKNS_2spINS_14SurfaceControlEEENS_2ui10LayerStackE")
                })
                .map(|p| {
                    core::mem::transmute::<
                        usize,
                        unsafe extern "C" fn(*mut c_void, *const *mut c_void, u32) -> *mut c_void,
                    >(p)
                });
            let tx_set_trusted_overlay_bool = libgui
                .sym("_ZN7android21SurfaceComposerClient11Transaction17setTrustedOverlayERKNS_2spINS_14SurfaceControlEEEb")
                .map(|p| {
                    core::mem::transmute::<
                        usize,
                        unsafe extern "C" fn(*mut c_void, *const *mut c_void, bool) -> *mut c_void,
                    >(p)
                });
            let tx_set_trusted_overlay_enum = libgui
                .sym("_ZN7android21SurfaceComposerClient11Transaction17setTrustedOverlayERKNS_2spINS_14SurfaceControlEEENS_3gui14TrustedOverlayE")
                .map(|p| {
                    core::mem::transmute::<
                        usize,
                        unsafe extern "C" fn(*mut c_void, *const *mut c_void, i32) -> *mut c_void,
                    >(p)
                });
            let tx_set_drop_input_mode = libgui
                .sym("_ZN7android21SurfaceComposerClient11Transaction16setDropInputModeERKNS_2spINS_14SurfaceControlEEENS_3gui13DropInputModeE")
                .map(|p| {
                    core::mem::transmute::<
                        usize,
                        unsafe extern "C" fn(*mut c_void, *const *mut c_void, u32) -> *mut c_void,
                    >(p)
                });
            let tx_show = libgui
                .sym("_ZN7android21SurfaceComposerClient11Transaction4showERKNS_2spINS_14SurfaceControlEEE")
                .map(|p| {
                    core::mem::transmute::<
                        usize,
                        unsafe extern "C" fn(*mut c_void, *const *mut c_void) -> *mut c_void,
                    >(p)
                });
            let tx_hide = libgui
                .sym("_ZN7android21SurfaceComposerClient11Transaction4hideERKNS_2spINS_14SurfaceControlEEE")
                .map(|p| {
                    core::mem::transmute::<
                        usize,
                        unsafe extern "C" fn(*mut c_void, *const *mut c_void) -> *mut c_void,
                    >(p)
                });

            let tx_apply = if let Some(p) =
                libgui.sym("_ZN7android21SurfaceComposerClient11Transaction5applyEbb")
            {
                TxApplyVariant::TwoBools(p)
            } else if let Some(p) =
                libgui.sym("_ZN7android21SurfaceComposerClient11Transaction5applyEb")
            {
                TxApplyVariant::OneBool(p)
            } else {
                return Err("missing Transaction::apply".to_string());
            };

            let anw_acquire =
                core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(
                    libandroid
                        .sym("ANativeWindow_acquire")
                        .ok_or("missing ANativeWindow_acquire")?,
                );
            let anw_release =
                core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(
                    libandroid
                        .sym("ANativeWindow_release")
                        .ok_or("missing ANativeWindow_release")?,
                );
            let anw_set_geometry = core::mem::transmute::<
                usize,
                unsafe extern "C" fn(*mut c_void, i32, i32, i32) -> i32,
            >(
                libandroid
                    .sym("ANativeWindow_setBuffersGeometry")
                    .ok_or("missing ANativeWindow_setBuffersGeometry")?,
            );
            let anw_lock = core::mem::transmute::<
                usize,
                unsafe extern "C" fn(*mut c_void, *mut ANativeWindowBuffer, *mut c_void) -> i32,
            >(
                libandroid
                    .sym("ANativeWindow_lock")
                    .ok_or("missing ANativeWindow_lock")?,
            );
            let anw_unlock_and_post =
                core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void) -> i32>(
                    libandroid
                        .sym("ANativeWindow_unlockAndPost")
                        .ok_or("missing ANativeWindow_unlockAndPost")?,
                );

            Ok(Self {
                _libutils: libutils,
                _libgui: libgui,
                _libandroid: libandroid,
                string8_ctor,
                string8_dtor,
                refbase_inc,
                refbase_dec,
                scc_ctor,
                scc_init_check,
                scc_create_surface,
                sc_get_surface,
                layer_meta_ctor,
                layer_meta_dtor,
                tx_ctor,
                tx_dtor,
                tx_set_layer,
                tx_set_pos,
                tx_set_flags,
                tx_set_layer_stack,
                tx_set_trusted_overlay_bool,
                tx_set_trusted_overlay_enum,
                tx_set_drop_input_mode,
                tx_show,
                tx_hide,
                tx_apply,
                anw_acquire,
                anw_release,
                anw_set_geometry,
                anw_lock,
                anw_unlock_and_post,
            })
        }
    }

    fn resolve_create_surface(libgui: &DlLib) -> Result<CreateSurfaceVariant, String> {
        let binder_candidates = [
            "_ZN7android21SurfaceComposerClient13createSurfaceERKNS_7String8EjjiiRKNS_2spINS_7IBinderEEENS_3gui13LayerMetadataEPj",
            "_ZN7android21SurfaceComposerClient13createSurfaceERKNS_7String8EjjijRKNS_2spINS_7IBinderEEENS_3gui13LayerMetadataEPj",
            "_ZN7android21SurfaceComposerClient13createSurfaceERKNS_7String8EjjijRKNS_2spINS_7IBinderEEENS_13LayerMetadataEPj",
            "_ZN7android21SurfaceComposerClient13createSurfaceERKNS_7String8EjjiiRKNS_2spINS_7IBinderEEENS_13LayerMetadataEPj",
        ];
        for sym in binder_candidates {
            if let Some(addr) = libgui.sym(sym) {
                return Ok(CreateSurfaceVariant::BinderParent(addr));
            }
        }

        let raw_candidates = [
            "_ZN7android21SurfaceComposerClient13createSurfaceERKNS_7String8EjjiiPNS_14SurfaceControlENS_3gui13LayerMetadataEPj",
            "_ZN7android21SurfaceComposerClient13createSurfaceERKNS_7String8EjjijPNS_14SurfaceControlENS_13LayerMetadataEPj",
            "_ZN7android21SurfaceComposerClient13createSurfaceERKNS_7String8EjjiiPNS_14SurfaceControlENS_13LayerMetadataEPj",
        ];
        for sym in raw_candidates {
            if let Some(addr) = libgui.sym(sym) {
                return Ok(CreateSurfaceVariant::RawControlParent(addr));
            }
        }

        let sniffed = find_elf64_dynsym_names("/system/lib64/libgui.so", |name| {
            name.starts_with("_ZN7android21SurfaceComposerClient13createSurfaceERKNS_7String8E")
        });
        for name in sniffed {
            if let Some(addr) = libgui.sym(&name) {
                if name.contains("IBinder") {
                    return Ok(CreateSurfaceVariant::BinderParent(addr));
                } else {
                    return Ok(CreateSurfaceVariant::RawControlParent(addr));
                }
            }
        }

        Err("unable to resolve SurfaceComposerClient::createSurface in libgui.so".to_string())
    }
}

/// AArch64 AAPCS64 `x8` (`sret`) trampoline for single-argument member functions returning `android::sp<T>`.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn call_sret_sp1(func: usize, this_x0: *mut c_void) -> *mut c_void {
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
unsafe fn call_sret_sp1(_func: usize, _this_x0: *mut c_void) -> *mut c_void {
    ptr::null_mut()
}

/// AArch64 AAPCS64 `x8` (`sret`) trampoline for `SurfaceComposerClient::createSurface`.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
#[allow(clippy::too_many_arguments)]
unsafe fn call_sret_create_surface(
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
unsafe fn call_sret_create_surface(
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
unsafe fn extract_native_window(surface_ptr: *mut c_void) -> Option<*mut c_void> {
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

/// Detects the native physical display viewport bounds (e.g., `2400x3392` Portrait panel on OnePlus Pad 3 Pro).
pub fn detect_native_display_bounds() -> ViewportBounds {
    // 1. Prefer DRM DSI connector mode (reports native panel Portrait resolution `2400x3392`).
    if let Ok(entries) = fs::read_dir("/sys/class/drm") {
        for e in entries.flatten() {
            let name = e.file_name();
            let s = name.to_string_lossy();
            if s.contains("DSI") || s.contains("dsi") || s.contains("panel") {
                let mode_path = e.path().join("modes");
                if let Ok(txt) = fs::read_to_string(mode_path) {
                    for line in txt.lines() {
                        let clean = line.trim().split(['-', '@', ' ']).next().unwrap_or("");
                        let mut it = clean.split('x');
                        if let (Some(a), Some(b)) = (it.next(), it.next())
                            && let (Ok(w), Ok(h)) = (a.parse::<u32>(), b.parse::<u32>())
                            && w >= 480
                            && h >= 480
                        {
                            return ViewportBounds::new(0.0, 0.0, w as f32, h as f32);
                        }
                    }
                }
            }
        }
    }

    // 2. Fallback to `/system/bin/wm size`.
    if let Ok(output) = std::process::Command::new("/system/bin/wm")
        .arg("size")
        .output()
    {
        let txt = String::from_utf8_lossy(&output.stdout);
        for line in txt.lines() {
            if let Some(pos) = line.find(':') {
                let val_part = line[pos + 1..].trim();
                let mut it = val_part.split('x');
                if let (Some(a), Some(b)) = (it.next(), it.next())
                    && let (Ok(w), Ok(h)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>())
                    && w >= 480
                    && h >= 480
                {
                    return ViewportBounds::new(0.0, 0.0, w as f32, h as f32);
                }
            }
        }
    }

    // 3. Default baseline for OnePlus Pad 3 Pro (2400x3392 native portrait frame).
    ViewportBounds::new(0.0, 0.0, 2400.0, 3392.0)
}

/// Represents a single `64x64` hardware sprite layer in `SurfaceFlinger`.
#[derive(Debug)]
struct SpriteLayer {
    control_ptr: *mut c_void,
    surface_ptr: *mut c_void,
    native_window: *mut c_void,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveSprite {
    None,
    Hover,
    Contact,
}

/// Hardware-accelerated dual-sprite overlay renderer backed by two pre-rendered `64x64` `SurfaceControl` layers.
#[derive(Debug)]
pub struct DualSpriteOverlay {
    syms: GuiSymbols,
    client_ptr: *mut c_void,
    hover_layer: SpriteLayer,
    contact_layer: SpriteLayer,
    active_sprite: ActiveSprite,
    last_x: f32,
    last_y: f32,
}

// SAFETY: `DualSpriteOverlay` exclusively owns its `SurfaceComposerClient` and `SurfaceControl` pointers.
unsafe impl Send for DualSpriteOverlay {}

impl DualSpriteOverlay {
    /// Creates the `SurfaceComposerClient` session and pre-renders both `64x64` `Hover` and `Contact` sprite layers.
    pub fn create(config: &Config) -> Result<Self, SurfaceError> {
        let hover_pixmap = render_hover_sprite(config);
        let contact_pixmap = render_contact_sprite(config);

        let syms = GuiSymbols::resolve()?;
        if let Ok(overlay) = Self::try_create(syms, &hover_pixmap, &contact_pixmap, false) {
            return Ok(overlay);
        }

        let syms_retry = GuiSymbols::resolve()?;
        Self::try_create(syms_retry, &hover_pixmap, &contact_pixmap, true).map_err(SurfaceError)
    }

    fn try_create(
        syms: GuiSymbols,
        hover_pixmap: &Pixmap,
        contact_pixmap: &Pixmap,
        use_system_euid: bool,
    ) -> Result<Self, String> {
        if use_system_euid {
            let _ = set_thread_euid(1000);
        }
        // SAFETY: Invokes `create_inner` with resolved `libgui` / `libutils` / `libandroid` symbols.
        let res = unsafe { Self::create_inner(syms, hover_pixmap, contact_pixmap) };
        if use_system_euid {
            let _ = set_thread_euid(0);
        }
        res
    }

    unsafe fn create_inner(
        syms: GuiSymbols,
        hover_pixmap: &Pixmap,
        contact_pixmap: &Pixmap,
    ) -> Result<Self, String> {
        // SAFETY: Constructs `SurfaceComposerClient` in a 256-byte aligned slab and increments its `RefBase` strong count.
        unsafe {
            let client_layout = std::alloc::Layout::from_size_align(256, 16).unwrap();
            let client_ptr = std::alloc::alloc_zeroed(client_layout).cast::<c_void>();
            if client_ptr.is_null() {
                return Err("alloc SurfaceComposerClient failed".to_string());
            }
            (syms.scc_ctor)(client_ptr);
            (syms.refbase_inc)(client_ptr, client_ptr);

            let status = (syms.scc_init_check)(client_ptr);
            if status != 0 {
                (syms.refbase_dec)(client_ptr, client_ptr);
                return Err(format!("SurfaceComposerClient::initCheck failed: {status}"));
            }

            let hover_layer = match Self::create_and_upload_sprite(
                &syms,
                client_ptr,
                "PenCursorHover",
                hover_pixmap,
            ) {
                Ok(layer) => layer,
                Err(e) => {
                    (syms.refbase_dec)(client_ptr, client_ptr);
                    return Err(e);
                }
            };

            let contact_layer = match Self::create_and_upload_sprite(
                &syms,
                client_ptr,
                "PenCursorContact",
                contact_pixmap,
            ) {
                Ok(layer) => layer,
                Err(e) => {
                    Self::destroy_sprite_layer(&syms, &hover_layer);
                    (syms.refbase_dec)(client_ptr, client_ptr);
                    return Err(e);
                }
            };

            Ok(Self {
                syms,
                client_ptr,
                hover_layer,
                contact_layer,
                active_sprite: ActiveSprite::None,
                last_x: f32::NAN,
                last_y: f32::NAN,
            })
        }
    }

    unsafe fn create_and_upload_sprite(
        syms: &GuiSymbols,
        client_ptr: *mut c_void,
        name: &str,
        pixmap: &Pixmap,
    ) -> Result<SpriteLayer, String> {
        // SAFETY: Creates a `64x64` `SurfaceControl`, locks its `ANativeWindow` once to upload the pre-rendered `tiny-skia` pixmap, and configures click-through flags.
        unsafe {
            let mut name_s8 = [0u64; 4];
            let c_name = CString::new(name).unwrap();
            (syms.string8_ctor)(name_s8.as_mut_ptr().cast(), c_name.as_ptr());

            let mut meta = LayerMetadataSlab::default();
            if let Some(ctor) = syms.layer_meta_ctor {
                ctor((&raw mut meta).cast());
            }

            let mut null_binder_sp: *mut c_void = ptr::null_mut();
            let mut transform_hint: u32 = 0;

            let control_ptr = match syms.scc_create_surface {
                CreateSurfaceVariant::BinderParent(fn_addr) => call_sret_create_surface(
                    fn_addr,
                    client_ptr,
                    name_s8.as_ptr().cast(),
                    SPRITE_SIZE,
                    SPRITE_SIZE,
                    PIXEL_FORMAT_RGBA_8888,
                    LAYER_FLAG_SKIP_SCREENSHOT,
                    (&raw mut null_binder_sp).cast(),
                    (&raw mut meta).cast(),
                    &raw mut transform_hint,
                ),
                CreateSurfaceVariant::RawControlParent(fn_addr) => call_sret_create_surface(
                    fn_addr,
                    client_ptr,
                    name_s8.as_ptr().cast(),
                    SPRITE_SIZE,
                    SPRITE_SIZE,
                    PIXEL_FORMAT_RGBA_8888,
                    LAYER_FLAG_SKIP_SCREENSHOT,
                    ptr::null(),
                    (&raw mut meta).cast(),
                    &raw mut transform_hint,
                ),
            };

            if let Some(dtor) = syms.layer_meta_dtor {
                dtor((&raw mut meta).cast());
            }
            (syms.string8_dtor)(name_s8.as_mut_ptr().cast());

            if control_ptr.is_null() {
                return Err(format!("createSurface({name}) returned null"));
            }

            let surface_ptr = call_sret_sp1(syms.sc_get_surface, control_ptr);
            if surface_ptr.is_null() {
                (syms.refbase_dec)(control_ptr, control_ptr);
                return Err(format!("getSurface({name}) returned null"));
            }

            let Some(native_window) = extract_native_window(surface_ptr) else {
                (syms.refbase_dec)(surface_ptr, surface_ptr);
                (syms.refbase_dec)(control_ptr, control_ptr);
                return Err(format!("failed to locate ANativeWindow magic for {name}"));
            };

            (syms.anw_acquire)(native_window);
            let _ = (syms.anw_set_geometry)(
                native_window,
                SPRITE_SIZE as c_int,
                SPRITE_SIZE as c_int,
                PIXEL_FORMAT_RGBA_8888,
            );

            // Upload the pre-rendered 64x64 RGBA8888 pixmap once via ANativeWindow_lock.
            let mut buf = ANativeWindowBuffer::default();
            let lock_rc = (syms.anw_lock)(native_window, &raw mut buf, ptr::null_mut());
            if lock_rc != 0 || buf.bits.is_null() {
                (syms.anw_release)(native_window);
                (syms.refbase_dec)(surface_ptr, surface_ptr);
                (syms.refbase_dec)(control_ptr, control_ptr);
                return Err(format!("ANativeWindow_lock({name}) failed: {lock_rc}"));
            }

            let src_bytes = pixmap.data();
            let dst_base = buf.bits.cast::<u8>();
            let row_bytes = (SPRITE_SIZE as usize) * 4;
            let dst_stride_bytes = (buf.stride.max(SPRITE_SIZE as i32) as usize) * 4;

            for y in 0..(SPRITE_SIZE as usize) {
                let src_row = src_bytes.as_ptr().add(y * row_bytes);
                let dst_row = dst_base.add(y * dst_stride_bytes);
                ptr::copy_nonoverlapping(src_row, dst_row, row_bytes);
            }

            let _ = (syms.anw_unlock_and_post)(native_window);

            // Configure Z-order, input pass-through (`DropInputMode::ALL`), trusted overlay, and initial hidden state.
            let mut tx = TransactionSlab::default();
            let tx_ptr = (&raw mut tx).cast::<c_void>();
            (syms.tx_ctor)(tx_ptr);

            Self::configure_layer_static_props(syms, tx_ptr, control_ptr);
            let bbq_child = *(control_ptr.cast::<u8>().add(0x58).cast::<*mut c_void>());
            if !bbq_child.is_null() {
                Self::configure_layer_static_props(syms, tx_ptr, bbq_child);
            }

            Self::apply_and_destroy_tx(syms, tx_ptr, true);

            Ok(SpriteLayer {
                control_ptr,
                surface_ptr,
                native_window,
            })
        }
    }

    unsafe fn configure_layer_static_props(
        syms: &GuiSymbols,
        tx_ptr: *mut c_void,
        sc_ptr: *mut c_void,
    ) {
        // SAFETY: Applies static layer properties on `sc_ptr` within an active `Transaction`.
        unsafe {
            let sc_ref = &raw const sc_ptr;
            let _ = (syms.tx_set_layer)(tx_ptr, sc_ref, TOPMOST_Z_ORDER);
            if let Some(set_pos) = syms.tx_set_pos {
                let _ = set_pos(tx_ptr, sc_ref, 0.0, 0.0);
            }
            if let Some(set_flags) = syms.tx_set_flags {
                let _ = set_flags(
                    tx_ptr,
                    sc_ref,
                    LAYER_FLAG_SKIP_SCREENSHOT,
                    LAYER_FLAG_SKIP_SCREENSHOT,
                );
            }
            if let Some(set_stack) = syms.tx_set_layer_stack {
                let _ = set_stack(tx_ptr, sc_ref, 0);
            }
            if let Some(set_drop) = syms.tx_set_drop_input_mode {
                let _ = set_drop(tx_ptr, sc_ref, 1 /* DropInputMode::ALL */);
            }
            if let Some(set_trusted) = syms.tx_set_trusted_overlay_enum {
                let _ = set_trusted(tx_ptr, sc_ref, 2 /* TrustedOverlay::ENABLED */);
            } else if let Some(set_trusted) = syms.tx_set_trusted_overlay_bool {
                let _ = set_trusted(tx_ptr, sc_ref, true);
            }
            if let Some(hide) = syms.tx_hide {
                let _ = hide(tx_ptr, sc_ref);
            }
        }
    }

    unsafe fn set_sprite_visibility_and_pos(
        syms: &GuiSymbols,
        tx_ptr: *mut c_void,
        layer: &SpriteLayer,
        visible: bool,
        pos: Option<(f32, f32)>,
    ) {
        // SAFETY: Updates position and visibility of `layer.control_ptr` and its `mBbqChild` in `tx_ptr`.
        unsafe {
            let ctrl_ref = &raw const layer.control_ptr;
            if let Some((x, y)) = pos
                && let Some(set_pos) = syms.tx_set_pos
            {
                let _ = set_pos(tx_ptr, ctrl_ref, x, y);
            }

            let bbq_child = *(layer
                .control_ptr
                .cast::<u8>()
                .add(0x58)
                .cast::<*mut c_void>());

            if visible {
                if let Some(show) = syms.tx_show {
                    let _ = show(tx_ptr, ctrl_ref);
                    if !bbq_child.is_null() {
                        let _ = show(tx_ptr, &raw const bbq_child);
                    }
                }
            } else if let Some(hide) = syms.tx_hide {
                let _ = hide(tx_ptr, ctrl_ref);
                if !bbq_child.is_null() {
                    let _ = hide(tx_ptr, &raw const bbq_child);
                }
            }
        }
    }

    unsafe fn apply_and_destroy_tx(syms: &GuiSymbols, tx_ptr: *mut c_void, one_way: bool) {
        // SAFETY: Commits `tx_ptr` using asynchronous (`oneWay`) Binder semantics and invokes `~Transaction()`.
        unsafe {
            match syms.tx_apply {
                TxApplyVariant::OneBool(f) => {
                    let func =
                        core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void, bool) -> i32>(
                            f,
                        );
                    let _ = func(tx_ptr, false);
                }
                TxApplyVariant::TwoBools(f) => {
                    let func = core::mem::transmute::<
                        usize,
                        unsafe extern "C" fn(*mut c_void, bool, bool) -> i32,
                    >(f);
                    let _ = func(tx_ptr, false, one_way);
                }
            }
            if let Some(dtor) = syms.tx_dtor {
                dtor(tx_ptr);
            }
        }
    }

    unsafe fn destroy_sprite_layer(syms: &GuiSymbols, layer: &SpriteLayer) {
        // SAFETY: Hides the layer and releases `ANativeWindow`, `Surface`, and `SurfaceControl` strong references.
        unsafe {
            if !layer.control_ptr.is_null() {
                let mut tx = TransactionSlab::default();
                let tx_ptr = (&raw mut tx).cast::<c_void>();
                (syms.tx_ctor)(tx_ptr);
                Self::set_sprite_visibility_and_pos(syms, tx_ptr, layer, false, None);
                Self::apply_and_destroy_tx(syms, tx_ptr, false);
            }
            if !layer.native_window.is_null() {
                (syms.anw_release)(layer.native_window);
            }
            if !layer.surface_ptr.is_null() {
                (syms.refbase_dec)(layer.surface_ptr, layer.surface_ptr);
            }
            if !layer.control_ptr.is_null() {
                (syms.refbase_dec)(layer.control_ptr, layer.control_ptr);
            }
        }
    }

    /// Updates the active sprite layer position and visibility via an asynchronous `SurfaceFlinger` transaction.
    pub fn update_cursor(&mut self, snapshot: &StylusSnapshot) {
        let desired = if !snapshot.in_range {
            ActiveSprite::None
        } else if snapshot.is_contact {
            ActiveSprite::Contact
        } else {
            ActiveSprite::Hover
        };

        if desired == ActiveSprite::None && self.active_sprite == ActiveSprite::None {
            return;
        }

        let top_left_x = snapshot.x - SPRITE_CENTER_OFFSET;
        let top_left_y = snapshot.y - SPRITE_CENTER_OFFSET;

        if desired == self.active_sprite
            && (top_left_x - self.last_x).abs() < 0.05
            && (top_left_y - self.last_y).abs() < 0.05
        {
            return;
        }

        // SAFETY: Constructs a `SurfaceComposerClient::Transaction` to reposition and toggle the pre-rendered sprite layers.
        unsafe {
            let mut tx = TransactionSlab::default();
            let tx_ptr = (&raw mut tx).cast::<c_void>();
            (self.syms.tx_ctor)(tx_ptr);

            match desired {
                ActiveSprite::None => {
                    Self::set_sprite_visibility_and_pos(
                        &self.syms,
                        tx_ptr,
                        &self.hover_layer,
                        false,
                        None,
                    );
                    Self::set_sprite_visibility_and_pos(
                        &self.syms,
                        tx_ptr,
                        &self.contact_layer,
                        false,
                        None,
                    );
                }
                ActiveSprite::Hover => {
                    Self::set_sprite_visibility_and_pos(
                        &self.syms,
                        tx_ptr,
                        &self.hover_layer,
                        true,
                        Some((top_left_x, top_left_y)),
                    );
                    if self.active_sprite != ActiveSprite::Hover {
                        Self::set_sprite_visibility_and_pos(
                            &self.syms,
                            tx_ptr,
                            &self.contact_layer,
                            false,
                            None,
                        );
                    }
                }
                ActiveSprite::Contact => {
                    Self::set_sprite_visibility_and_pos(
                        &self.syms,
                        tx_ptr,
                        &self.contact_layer,
                        true,
                        Some((top_left_x, top_left_y)),
                    );
                    if self.active_sprite != ActiveSprite::Contact {
                        Self::set_sprite_visibility_and_pos(
                            &self.syms,
                            tx_ptr,
                            &self.hover_layer,
                            false,
                            None,
                        );
                    }
                }
            }

            Self::apply_and_destroy_tx(&self.syms, tx_ptr, true);
        }

        self.active_sprite = desired;
        self.last_x = top_left_x;
        self.last_y = top_left_y;
    }
}

impl OverlayBackend for DualSpriteOverlay {
    type Error = SurfaceError;

    #[inline]
    fn render_frame(
        &mut self,
        snapshot: &StylusSnapshot,
        _config: &Config,
    ) -> Result<(), Self::Error> {
        self.update_cursor(snapshot);
        Ok(())
    }
}

impl Drop for DualSpriteOverlay {
    fn drop(&mut self) {
        // SAFETY: Cleans up both sprite layers and decrements the `SurfaceComposerClient` strong reference.
        unsafe {
            Self::destroy_sprite_layer(&self.syms, &self.hover_layer);
            Self::destroy_sprite_layer(&self.syms, &self.contact_layer);
            if !self.client_ptr.is_null() {
                (self.syms.refbase_dec)(self.client_ptr, self.client_ptr);
                self.client_ptr = ptr::null_mut();
            }
        }
    }
}
