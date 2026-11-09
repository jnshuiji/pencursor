//! PenCursor cross-platform binary entrypoint.
//!
//! Dispatches execution to the target platform runtime backend (`WindowsRuntime` on Windows,
//! or `AndroidRuntime` on Android KernelSU daemon targets).

use pencursor_core::{Config, PlatformRuntime};

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use pencursor_windows::WindowsRuntime;

    let config = Config::load_or_default(Config::resolve_config_path());
    WindowsRuntime::run(config)?;
    Ok(())
}

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use pencursor_android::AndroidRuntime;

    let config = Config::load_or_default(Config::resolve_config_path());
    AndroidRuntime::run(config)?;
    Ok(())
}
