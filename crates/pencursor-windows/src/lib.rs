//! `pencursor-windows` — Windows hardware-accelerated backend for PenCursor.
//!
//! Encapsulates Win32 `RawInput` (HID Digitizer and absolute mouse packets), the `WH_MOUSE_LL`
//! low-level hook, and the Direct3D 11 + DXGI + DirectComposition + Direct2D transparent overlay pipeline.

#![cfg_attr(not(windows), allow(unused))]

#[cfg(windows)]
pub mod hid;
#[cfg(windows)]
pub mod hook;
#[cfg(windows)]
pub mod input;
#[cfg(windows)]
pub mod overlay;
#[cfg(windows)]
pub mod runtime;

#[cfg(windows)]
pub use hook::MouseHookGuard;
#[cfg(windows)]
pub use overlay::OverlayRenderer;
#[cfg(windows)]
pub use runtime::{PrecisionTimerGuard, WindowsRuntime};
