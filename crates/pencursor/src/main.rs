//! PenCursor cross-platform binary entrypoint.
//!
//! Dispatches execution to the target platform runtime backend (`WindowsRuntime` on Windows,
//! or the Android KernelSU daemon backend on Android targets).

use pencursor_core::Config;

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use pencursor_core::PlatformRuntime;
    use pencursor_windows::WindowsRuntime;

    let config = Config::default();
    WindowsRuntime::run(config)?;
    Ok(())
}

#[cfg(not(windows))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _config = Config::default();
    eprintln!("[PenCursor] 当前 Target 非 Windows 平台，Android KernelSU Daemon Backend 待接入。");
    Ok(())
}
