mod config;
mod hid;
mod overlay;
mod stylus;

use config::Config;
use overlay::OverlayRenderer;
use std::time::Duration;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

windows::core::link!("winmm.dll" "system" fn timeBeginPeriod(uperiod: u32) -> u32);
windows::core::link!("winmm.dll" "system" fn timeEndPeriod(uperiod: u32) -> u32);

/// RAII 高精度多媒体时钟保护器
///
/// 将 Windows 默认 15.6ms 时钟中断精度提升至 1ms，在退出时自动恢复系统默认值。
struct PrecisionTimerGuard(u32);

impl PrecisionTimerGuard {
    fn new(period_ms: u32) -> Self {
        unsafe {
            timeBeginPeriod(period_ms);
        }
        Self(period_ms)
    }
}

impl Drop for PrecisionTimerGuard {
    fn drop(&mut self) {
        unsafe {
            timeEndPeriod(self.0);
        }
    }
}

fn main() -> Result<()> {
    // 1. 启用系统 Per-Monitor DPI Awareness V2 高 DPI 感知
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // 2. 启用 1ms 高精度系统定时器调度
    let _timer_guard = PrecisionTimerGuard::new(1);

    // 3. 输出专业控制台信息
    println!("========================================================");
    println!("  PenCursor - 高性能手写笔光标增强工具");
    println!("========================================================");
    println!("• 全局捕获：双轨 Raw Input (HID Digitizer + Absolute Mouse)");
    println!("• 极低延迟：DXGI 单帧队列 + DirectComposition 零拷贝合成");
    println!("• 智能感知：手写笔悬空唤醒，实体鼠标移动自动休眠");
    println!("• 触碰收敛：落笔瞬间收缩微点，悬空时高对比度矢量对齐");
    println!("• 退出方式：按 Ctrl+C 或关闭本窗口");
    println!("--------------------------------------------------------");

    let config = Config::default();

    // 4. 启动后台低级鼠标监听线程
    let _hook_handle = stylus::start_mouse_hook()?;
    println!("[Info] 手写笔全局监听服务就绪");

    // 5. 初始化硬件加速全屏透明覆层
    let mut renderer = OverlayRenderer::new()?;
    println!("[Info] GPU 硬件加速透明覆层已启动");
    println!("[Info] 服务正常运行中...\n");

    let mut msg = MSG::default();

    // 6. 主渲染与消息调度循环
    loop {
        // 非阻塞式消费 Windows 窗口消息
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return Ok(());
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        let snapshot = stylus::get_snapshot(config.hover_timeout_ms);
        renderer.render_frame(&snapshot, &config)?;

        if !snapshot.in_range {
            // 空闲状态：休眠 10ms 降低 CPU 占用（接近 0.0%）
            std::thread::sleep(Duration::from_millis(10));
        } else {
            // 活跃悬空/触控：1ms 高精度微休眠，提供 500Hz+ 高刷新率渲染
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
