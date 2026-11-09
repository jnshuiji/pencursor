//! `pencursor-android` — Android KernelSU daemon backend for PenCursor.
//!
//! Combines `tiny-skia` vector sprite pre-rendering, dual `64x64` `SurfaceFlinger` hardware
//! overlay layers (`libgui.so` + `libandroid.so`), `epoll`/`inotify` `evdev` batch coalescing,
//! and event-driven sysfs USB runtime power management.

#[cfg(unix)]
pub mod elf;
#[cfg(unix)]
pub mod evdev;
#[cfg(unix)]
pub mod power;
pub mod raster;
#[cfg(unix)]
pub mod runtime;
#[cfg(unix)]
pub mod surface;
#[cfg(unix)]
pub mod sys;

pub use raster::{SPRITE_CENTER_OFFSET, SPRITE_SIZE, render_contact_sprite, render_hover_sprite};
#[cfg(unix)]
pub use runtime::{AndroidRuntime, AndroidRuntimeError};
#[cfg(unix)]
pub use surface::{DualSpriteOverlay, SurfaceError, detect_native_display_bounds};
