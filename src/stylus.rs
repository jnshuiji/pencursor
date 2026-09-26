//! 手写笔全局状态监听与管理模块
//!
//! 维护全局原子状态（坐标、悬空、触碰、最后更新时间），并运行底层鼠标钩子（WH_MOUSE_LL）
//! 用于即时捕获手写笔与实体鼠标的活动切换。

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Windows Tablet / Stylus 硬件消息特征掩码
const MI_WP_SIGNATURE: usize = 0xFF515700;
const SIGNATURE_MASK: usize = 0xFFFFFF00;
const TOUCH_MASK: usize = 0x80;

static START_INSTANT: LazyLock<Instant> = LazyLock::new(Instant::now);

static STYLUS_X: AtomicU32 = AtomicU32::new(0);
static STYLUS_Y: AtomicU32 = AtomicU32::new(0);
static STYLUS_IN_RANGE: AtomicBool = AtomicBool::new(false);
static STYLUS_IS_CONTACT: AtomicBool = AtomicBool::new(false);
static STYLUS_LAST_UPDATE_MS: AtomicU64 = AtomicU64::new(0);

/// 手写笔当前瞬时状态快照
#[derive(Debug, Clone, Copy)]
pub struct StylusSnapshot {
    /// 虚拟屏幕全局 X 坐标（像素）
    pub x: f32,
    /// 虚拟屏幕全局 Y 坐标（像素）
    pub y: f32,
    /// 手写笔是否在屏幕感应范围内（悬空或触屏）
    pub in_range: bool,
    /// 笔尖是否处于触屏接触状态
    pub is_contact: bool,
}

/// 获取自进程启动以来的单调毫秒数
pub fn now_ms() -> u64 {
    START_INSTANT.elapsed().as_millis() as u64
}

/// 更新手写笔活跃状态与坐标
///
/// 当 `is_contact` 为 `Some(state)` 时同步更新触碰状态；
/// 为 `None` 时仅更新坐标，保持原有的硬件接触状态不变。
pub fn mark_pen_active(x: f32, y: f32, is_contact: Option<bool>) {
    STYLUS_X.store(x.to_bits(), Ordering::Relaxed);
    STYLUS_Y.store(y.to_bits(), Ordering::Relaxed);
    if let Some(contact) = is_contact {
        STYLUS_IS_CONTACT.store(contact, Ordering::Relaxed);
    }
    STYLUS_IN_RANGE.store(true, Ordering::Relaxed);
    STYLUS_LAST_UPDATE_MS.store(now_ms(), Ordering::Relaxed);
}

/// 标记实体鼠标发生物理移动
///
/// 若手写笔在最近的超时时间内未收到更新，则自动让出手写笔活跃状态，隐匿光标。
pub fn mark_mouse_active(timeout_ms: u64) {
    let now = now_ms();
    let last_pen = STYLUS_LAST_UPDATE_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last_pen) > timeout_ms {
        STYLUS_IN_RANGE.store(false, Ordering::Relaxed);
        STYLUS_IS_CONTACT.store(false, Ordering::Relaxed);
    }
}

/// 查询手写笔是否在感应范围内
pub fn is_pen_in_range(timeout_ms: u64) -> bool {
    let now = now_ms();
    let last = STYLUS_LAST_UPDATE_MS.load(Ordering::Relaxed);
    now.saturating_sub(last) < timeout_ms && STYLUS_IN_RANGE.load(Ordering::Relaxed)
}

/// 获取当前手写笔的完整状态快照
pub fn get_snapshot(timeout_ms: u64) -> StylusSnapshot {
    let in_range = is_pen_in_range(timeout_ms);

    let x = f32::from_bits(STYLUS_X.load(Ordering::Relaxed));
    let y = f32::from_bits(STYLUS_Y.load(Ordering::Relaxed));

    let is_contact = if in_range {
        let key_down = unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 };
        key_down || STYLUS_IS_CONTACT.load(Ordering::Relaxed)
    } else {
        false
    };

    StylusSnapshot {
        x,
        y,
        in_range,
        is_contact,
    }
}

/// 底层全局鼠标钩子回调过程
unsafe extern "system" fn low_level_mouse_proc(
    n_code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if n_code >= 0 {
        let ms = unsafe { &*(l_param.0 as *const MSLLHOOKSTRUCT) };
        let extra = ms.dwExtraInfo;
        let is_tablet = (extra & SIGNATURE_MASK) == MI_WP_SIGNATURE;
        let is_pen = is_tablet && ((extra & TOUCH_MASK) == 0);

        if is_pen {
            let x = ms.pt.x as f32;
            let y = ms.pt.y as f32;
            let msg = w_param.0 as u32;

            let contact_update = match msg {
                WM_LBUTTONDOWN => Some(true),
                WM_LBUTTONUP => Some(false),
                _ => None,
            };

            mark_pen_active(x, y, contact_update);
        } else if !is_tablet {
            // 实体物理鼠标移动，触发鼠标互斥休眠（默认 100ms 阈值）
            mark_mouse_active(100);
        }
    }

    unsafe { CallNextHookEx(None, n_code, w_param, l_param) }
}

/// 启动全局低级鼠标监听线程
pub fn start_mouse_hook() -> Result<std::thread::JoinHandle<()>, windows::core::Error> {
    let handle = std::thread::Builder::new()
        .name("pen_input_hook".into())
        .spawn(|| unsafe {
            let h_module = GetModuleHandleW(None).unwrap_or_default();
            let hook = match SetWindowsHookExW(
                WH_MOUSE_LL,
                Some(low_level_mouse_proc),
                Some(h_module.into()),
                0,
            ) {
                Ok(h) => h,
                Err(err) => {
                    eprintln!("[Error] 安装 WH_MOUSE_LL 钩子失败: {:?}", err);
                    return;
                }
            };

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            let _ = UnhookWindowsHookEx(hook);
        })
        .map_err(|e| windows::core::Error::new(E_FAIL, e.to_string()))?;

    Ok(handle)
}
