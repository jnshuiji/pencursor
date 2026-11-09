//! Dynamic symbol resolution for `libutils.so`, `libgui.so`, and `libandroid.so`.

use super::abi::{ANativeWindowBuffer, CreateSurfaceVariant, TxApplyVariant};
use crate::elf::find_elf64_dynsym_names;
use crate::sys::DlLib;
use std::ffi::{c_char, c_void};

/// Dynamically resolved symbol table from `libutils.so`, `libgui.so`, and `libandroid.so`.
#[derive(Debug)]
pub(super) struct GuiSymbols {
    _libutils: DlLib,
    _libgui: DlLib,
    _libandroid: DlLib,

    pub(super) string8_ctor: unsafe extern "C" fn(*mut c_void, *const c_char),
    pub(super) string8_dtor: unsafe extern "C" fn(*mut c_void),
    pub(super) refbase_inc: unsafe extern "C" fn(*mut c_void, *const c_void),
    pub(super) refbase_dec: unsafe extern "C" fn(*mut c_void, *const c_void),

    pub(super) scc_ctor: unsafe extern "C" fn(*mut c_void),
    pub(super) scc_init_check: unsafe extern "C" fn(*const c_void) -> i32,
    pub(super) scc_create_surface: CreateSurfaceVariant,
    pub(super) sc_get_surface: usize,

    pub(super) layer_meta_ctor: Option<unsafe extern "C" fn(*mut c_void)>,
    pub(super) layer_meta_dtor: Option<unsafe extern "C" fn(*mut c_void)>,

    pub(super) tx_ctor: unsafe extern "C" fn(*mut c_void),
    pub(super) tx_dtor: Option<unsafe extern "C" fn(*mut c_void)>,
    pub(super) tx_set_layer:
        unsafe extern "C" fn(*mut c_void, *const *mut c_void, i32) -> *mut c_void,
    pub(super) tx_set_pos:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void, f32, f32) -> *mut c_void>,
    pub(super) tx_set_flags:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void, u32, u32) -> *mut c_void>,
    pub(super) tx_set_layer_stack:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void, u32) -> *mut c_void>,
    pub(super) tx_set_trusted_overlay_bool:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void, bool) -> *mut c_void>,
    pub(super) tx_set_trusted_overlay_enum:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void, i32) -> *mut c_void>,
    pub(super) tx_set_drop_input_mode:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void, u32) -> *mut c_void>,
    pub(super) tx_show:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void) -> *mut c_void>,
    pub(super) tx_hide:
        Option<unsafe extern "C" fn(*mut c_void, *const *mut c_void) -> *mut c_void>,
    pub(super) tx_apply: TxApplyVariant,

    pub(super) anw_acquire: unsafe extern "C" fn(*mut c_void),
    pub(super) anw_release: unsafe extern "C" fn(*mut c_void),
    pub(super) anw_set_geometry: unsafe extern "C" fn(*mut c_void, i32, i32, i32) -> i32,
    pub(super) anw_lock:
        unsafe extern "C" fn(*mut c_void, *mut ANativeWindowBuffer, *mut c_void) -> i32,
    pub(super) anw_unlock_and_post: unsafe extern "C" fn(*mut c_void) -> i32,
}

impl GuiSymbols {
    pub(super) fn resolve() -> Result<Self, String> {
        let libutils =
            DlLib::open("/system/lib64/libutils.so").or_else(|_| DlLib::open("libutils.so"))?;
        let libgui =
            DlLib::open("/system/lib64/libgui.so").or_else(|_| DlLib::open("libgui.so"))?;
        let libandroid =
            DlLib::open("/system/lib64/libandroid.so").or_else(|_| DlLib::open("libandroid.so"))?;

        // SAFETY: All transmuted function pointers match AOSP Itanium C++ ABI and NDK C ABI signatures.
        unsafe {
            let string8_ctor =
                core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void, *const c_char)>(
                    libutils
                        .sym("_ZN7android7String8C1EPKc")
                        .or_else(|| libutils.sym("_ZN7android7String8C2EPKc"))
                        .ok_or("missing String8::String8(char const*)")?,
                );
            let string8_dtor = core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(
                libutils
                    .sym("_ZN7android7String8D1Ev")
                    .or_else(|| libutils.sym("_ZN7android7String8D2Ev"))
                    .ok_or("missing String8::~String8()")?,
            );
            let refbase_inc =
                core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void, *const c_void)>(
                    libutils
                        .sym("_ZNK7android7RefBase9incStrongEPKv")
                        .ok_or("missing RefBase::incStrong")?,
                );
            let refbase_dec =
                core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void, *const c_void)>(
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

            let anw_acquire = core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(
                libandroid
                    .sym("ANativeWindow_acquire")
                    .ok_or("missing ANativeWindow_acquire")?,
            );
            let anw_release = core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(
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
