//! Windows runtime scheduler, high-resolution timer guard, and Win32 message loop.

use crate::hook::MouseHookGuard;
use crate::input::current_snapshot;
use crate::overlay::OverlayRenderer;
use pencursor_core::{Config, OverlayBackend, PlatformRuntime, global_tracker};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::Result;

windows::core::link!("winmm.dll" "system" fn timeBeginPeriod(uperiod: u32) -> u32);
windows::core::link!("winmm.dll" "system" fn timeEndPeriod(uperiod: u32) -> u32);

/// RAII guard for the Windows multimedia timer resolution (`timeBeginPeriod` / `timeEndPeriod`).
///
/// Elevates system timer interrupt resolution (e.g., from 15.6ms to 1ms) while alive and
/// restores the previous resolution on drop.
#[derive(Debug)]
pub struct PrecisionTimerGuard(u32);

impl PrecisionTimerGuard {
    /// Requests a system timer resolution of `period_ms` milliseconds.
    pub fn new(period_ms: u32) -> Self {
        // SAFETY: Calls `winmm.dll` `timeBeginPeriod`; paired with `timeEndPeriod` in `Drop`.
        unsafe {
            timeBeginPeriod(period_ms);
        }
        Self(period_ms)
    }
}

impl Drop for PrecisionTimerGuard {
    fn drop(&mut self) {
        // SAFETY: Restores the exact timer period requested in `PrecisionTimerGuard::new`.
        unsafe {
            timeEndPeriod(self.0);
        }
    }
}

/// Windows platform runtime implementation.
#[derive(Debug, Default)]
pub struct WindowsRuntime;

impl PlatformRuntime for WindowsRuntime {
    type Error = windows::core::Error;

    fn run(config: Config) -> Result<()> {
        // SAFETY: Enables Per-Monitor DPI Awareness V2 prior to creating any Win32 window.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }

        let _timer_guard = PrecisionTimerGuard::new(1);

        global_tracker().configure_from(&config);

        println!("========================================================");
        println!("  PenCursor - 高性能 Stylus Cursor Overlay 服务");
        println!("========================================================");
        println!("• 全局捕获：双轨 RawInput (HID Digitizer + Absolute Mouse)");
        println!("• 极低延迟：DXGI 单帧队列 + DirectComposition 零拷贝合成");
        println!("• 智能感知：Stylus Hover 唤醒，Mouse 移动自动休眠");
        println!("• 触碰收敛：Contact 瞬间收缩微点，Hover 时高对比度矢量对齐");
        println!("• 退出方式：按 Ctrl+C 或关闭本窗口");
        println!("--------------------------------------------------------");

        let _hook_guard = MouseHookGuard::start()?;
        println!("[Info] Stylus 全局监听服务就绪");

        let mut renderer = OverlayRenderer::with_palette(config.palette)?;
        println!("[Info] GPU DirectComposition Overlay 已启动");
        println!("[Info] 服务正常运行中...\n");

        let mut msg = MSG::default();

        loop {
            // SAFETY: Non-blocking Win32 message pump consuming queued messages for the current thread.
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return Ok(());
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            let snapshot = current_snapshot(config.hover_timeout_ms);
            OverlayBackend::render_frame(&mut renderer, &snapshot, &config)?;

            std::thread::sleep(config.frame_sleep_duration(snapshot.in_range));
        }
    }
}
