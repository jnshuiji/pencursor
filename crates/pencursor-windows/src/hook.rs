//! Global Win32 low-level mouse hook (`WH_MOUSE_LL`) management.
//!
//! Spawns a dedicated background message pump thread to intercept system-wide stylus and
//! physical mouse transitions in real time. [`MouseHookGuard`] provides RAII lifecycle
//! cleanup by posting `WM_QUIT` and joining the hook thread on drop.

use crate::input::{is_pen_signature, is_tablet_signature};
use pencursor_core::global_tracker;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::Error;

/// Win32 `WH_MOUSE_LL` hook callback procedure.
unsafe extern "system" fn low_level_mouse_proc(
    n_code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if n_code >= 0 && l_param.0 != 0 {
        // SAFETY: When `n_code >= 0`, the kernel guarantees `l_param` points to a valid `MSLLHOOKSTRUCT`.
        let ms = unsafe { &*(l_param.0 as *const MSLLHOOKSTRUCT) };
        let extra = ms.dwExtraInfo;
        let tracker = global_tracker();

        if is_pen_signature(extra) {
            let x = ms.pt.x as f32;
            let y = ms.pt.y as f32;
            let msg = w_param.0 as u32;

            let contact_update = match msg {
                WM_LBUTTONDOWN => Some(true),
                WM_LBUTTONUP => Some(false),
                _ => None,
            };

            tracker.mark_pen_active(x, y, contact_update);
        } else if !is_tablet_signature(extra) {
            tracker.mark_mouse_active();
        }
    }

    // SAFETY: Forwards the hook notification to the next handler in the Win32 hook chain.
    unsafe { CallNextHookEx(None, n_code, w_param, l_param) }
}

/// RAII guard managing the background `WH_MOUSE_LL` listener thread.
#[derive(Debug)]
pub struct MouseHookGuard {
    thread_id: Arc<AtomicU32>,
    join_handle: Option<JoinHandle<()>>,
}

impl MouseHookGuard {
    /// Spawns the background message loop thread and installs the `WH_MOUSE_LL` hook.
    pub fn start() -> windows::core::Result<Self> {
        let thread_id = Arc::new(AtomicU32::new(0));
        let thread_id_clone = Arc::clone(&thread_id);

        let handle = std::thread::Builder::new()
            .name("pen_input_hook".into())
            .spawn(move || {
                // SAFETY: Records the Win32 thread ID, installs the `WH_MOUSE_LL` hook, and runs `GetMessageW` until `WM_QUIT`.
                unsafe {
                    thread_id_clone.store(GetCurrentThreadId(), Ordering::Release);

                    let h_module = GetModuleHandleW(None).unwrap_or_default();
                    let hook = match SetWindowsHookExW(
                        WH_MOUSE_LL,
                        Some(low_level_mouse_proc),
                        Some(h_module.into()),
                        0,
                    ) {
                        Ok(h) => h,
                        Err(err) => {
                            eprintln!("[Error] 注册 WH_MOUSE_LL Hook 失败: {err:?}");
                            return;
                        }
                    };

                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }

                    let _ = UnhookWindowsHookEx(hook);
                }
            })
            .map_err(|e| Error::new(E_FAIL, e.to_string()))?;

        Ok(Self {
            thread_id,
            join_handle: Some(handle),
        })
    }
}

impl Drop for MouseHookGuard {
    fn drop(&mut self) {
        let tid = self.thread_id.load(Ordering::Acquire);
        if tid != 0 {
            // SAFETY: Posts `WM_QUIT` to the hook worker thread so `GetMessageW` returns and unhooks cleanly.
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}
