//! Native `SurfaceFlinger` dual-sprite overlay backend (`libgui.so` + `libandroid.so`).
//!
//! Architecture:
//! - Creates two `64x64` topmost click-through `SurfaceControl` sprite layers (`hover_layer`
//!   and `contact_layer`).
//! - Rasterizes and uploads the `Hover` and `Contact` vector sprites at initialization and
//!   on configuration reloads using Linebender `vello_cpu` and `ANativeWindow_lock` /
//!   `ANativeWindow_unlockAndPost`.
//! - Performs zero runtime pixel writes during motion: all cursor movement and
//!   hover/contact transitions are executed exclusively via asynchronous (`oneWay`)
//!   `SurfaceComposerClient::Transaction` (`setPosition`, `show`, `hide`).

mod abi;
mod symbols;
mod viewport;

pub use viewport::detect_native_display_bounds;

use crate::sys::set_thread_euid;
use abi::{
    ANativeWindowBuffer, CreateSurfaceVariant, LAYER_FLAG_SKIP_SCREENSHOT, LayerMetadataSlab,
    PIXEL_FORMAT_RGBA_8888, TOPMOST_Z_ORDER, TransactionSlab, TxApplyVariant,
    call_sret_create_surface, call_sret_sp1, extract_native_window,
};
use pencursor_core::{
    Config, OverlayBackend, Pixmap, SPRITE_CENTER_OFFSET, SPRITE_SIZE, StylusSnapshot,
    render_contact_sprite, render_hover_sprite,
};
use std::ffi::{CString, c_int, c_void};
use std::fmt;
use std::ptr;
use symbols::GuiSymbols;

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
        let sprite_size_u32 = u32::from(SPRITE_SIZE);
        // SAFETY: Creates a `64x64` `SurfaceControl`, locks its `ANativeWindow` once to upload the pre-rendered `vello_cpu` pixmap, and configures click-through flags.
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
                    sprite_size_u32,
                    sprite_size_u32,
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
                    sprite_size_u32,
                    sprite_size_u32,
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
                sprite_size_u32 as c_int,
                sprite_size_u32 as c_int,
                PIXEL_FORMAT_RGBA_8888,
            );

            if let Err(e) = Self::upload_pixmap_to_window(syms, native_window, name, pixmap) {
                (syms.anw_release)(native_window);
                (syms.refbase_dec)(surface_ptr, surface_ptr);
                (syms.refbase_dec)(control_ptr, control_ptr);
                return Err(e);
            }

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

    unsafe fn upload_pixmap_to_window(
        syms: &GuiSymbols,
        native_window: *mut c_void,
        name: &str,
        pixmap: &Pixmap,
    ) -> Result<(), String> {
        let sprite_size_u32 = u32::from(SPRITE_SIZE);
        // SAFETY: Locks `native_window`, copies the 64x64 premultiplied RGBA8888 pixmap into the graphic buffer, and posts it.
        unsafe {
            let mut buf = ANativeWindowBuffer::default();
            let lock_rc = (syms.anw_lock)(native_window, &raw mut buf, ptr::null_mut());
            if lock_rc != 0 || buf.bits.is_null() {
                return Err(format!("ANativeWindow_lock({name}) failed: {lock_rc}"));
            }

            let src_bytes = pixmap.data_as_u8_slice();
            let dst_base = buf.bits.cast::<u8>();
            let row_bytes = usize::from(SPRITE_SIZE) * 4;
            let dst_stride_bytes = (buf.stride.max(sprite_size_u32 as i32) as usize) * 4;

            for y in 0..usize::from(SPRITE_SIZE) {
                let src_row = src_bytes.as_ptr().add(y * row_bytes);
                let dst_row = dst_base.add(y * dst_stride_bytes);
                ptr::copy_nonoverlapping(src_row, dst_row, row_bytes);
            }

            let _ = (syms.anw_unlock_and_post)(native_window);
            Ok(())
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
                    let func = core::mem::transmute::<
                        usize,
                        unsafe extern "C" fn(*mut c_void, bool) -> i32,
                    >(f);
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

    /// Re-rasterizes the `64x64` `Hover` and `Contact` sprites with `vello_cpu` and uploads them
    /// in-place to the active `SurfaceControl` `ANativeWindow` buffers without recreating layers.
    pub fn reload_sprites(&mut self, config: &Config) -> Result<(), SurfaceError> {
        let hover_pixmap = render_hover_sprite(config);
        let contact_pixmap = render_contact_sprite(config);

        // SAFETY: `self.hover_layer.native_window` and `self.contact_layer.native_window` are valid acquired `ANativeWindow` pointers.
        unsafe {
            Self::upload_pixmap_to_window(
                &self.syms,
                self.hover_layer.native_window,
                "PenCursorHover",
                &hover_pixmap,
            )
            .map_err(SurfaceError)?;
            Self::upload_pixmap_to_window(
                &self.syms,
                self.contact_layer.native_window,
                "PenCursorContact",
                &contact_pixmap,
            )
            .map_err(SurfaceError)?;
        }

        if !config.overlay_enabled {
            let hidden = StylusSnapshot {
                in_range: false,
                ..StylusSnapshot::default()
            };
            self.update_cursor(&hidden);
        } else {
            // Force redraw/recommit on next coordinate update by invalidating cached sprite state
            self.active_sprite = ActiveSprite::None;
            self.last_x = f32::NAN;
            self.last_y = f32::NAN;
        }

        Ok(())
    }
}

impl OverlayBackend for DualSpriteOverlay {
    type Error = SurfaceError;

    #[inline]
    fn render_frame(
        &mut self,
        snapshot: &StylusSnapshot,
        config: &Config,
    ) -> Result<(), Self::Error> {
        if !config.overlay_enabled {
            let hidden = StylusSnapshot {
                in_range: false,
                ..*snapshot
            };
            self.update_cursor(&hidden);
        } else {
            self.update_cursor(snapshot);
        }
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
