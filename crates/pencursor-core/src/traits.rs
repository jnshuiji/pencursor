//! Cross-platform backend traits (Ports & Adapters).
//!
//! Defines the hardware-accelerated overlay rendering contract ([`OverlayBackend`]) and the
//! platform service lifecycle contract ([`PlatformRuntime`]) shared across Windows
//! (Win32 RawInput + DirectComposition/Direct2D) and Android (KernelSU `evdev` + Surface overlay).

use crate::config::Config;
use crate::state::StylusSnapshot;
use std::error::Error;

/// Hardware-accelerated transparent overlay rendering backend interface.
pub trait OverlayBackend {
    /// Platform-specific rendering error type.
    type Error: Error + Send + Sync + 'static;

    /// Renders a single overlay frame reflecting the provided [`StylusSnapshot`] and [`Config`].
    fn render_frame(
        &mut self,
        snapshot: &StylusSnapshot,
        config: &Config,
    ) -> Result<(), Self::Error>;
}

/// Platform daemon / application runtime lifecycle interface.
pub trait PlatformRuntime {
    /// Platform-specific runtime error type.
    type Error: Error + Send + Sync + 'static;

    /// Initializes platform input listeners and the overlay rendering loop, blocking until exit.
    fn run(config: Config) -> Result<(), Self::Error>;
}
