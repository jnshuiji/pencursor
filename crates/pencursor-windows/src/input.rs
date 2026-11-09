//! Win32 `RawInput` message dispatcher, hardware signature masks, and virtual desktop metrics.

use crate::hid::parse_digitizer_report;
use pencursor_core::{DeviceAxisRange, StylusSnapshot, ViewportBounds, global_tracker};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Input::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Windows Ink / Tablet message signature constants in `dwExtraInfo` / `ulExtraInformation`.
pub const MI_WP_SIGNATURE: usize = 0xFF51_5700;
pub const SIGNATURE_MASK: usize = 0xFFFF_FF00;
pub const TOUCH_MASK: usize = 0x80;

/// Returns `true` if `extra` carries the Windows Pointer / Tablet signature prefix.
#[inline]
pub const fn is_tablet_signature(extra: usize) -> bool {
    (extra & SIGNATURE_MASK) == MI_WP_SIGNATURE
}

/// Returns `true` if `extra` originates from a stylus transducer (excluding finger touch).
#[inline]
pub const fn is_pen_signature(extra: usize) -> bool {
    is_tablet_signature(extra) && ((extra & TOUCH_MASK) == 0)
}

/// Queries the current multi-monitor virtual desktop bounding rectangle.
#[inline]
pub fn current_virtual_screen_bounds() -> ViewportBounds {
    // SAFETY: `GetSystemMetrics` is a stateless, thread-safe Win32 query.
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN) as f32;
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN) as f32;
        let vcx = GetSystemMetrics(SM_CXVIRTUALSCREEN) as f32;
        let vcy = GetSystemMetrics(SM_CYVIRTUALSCREEN) as f32;
        ViewportBounds::new(vx, vy, vcx, vcy)
    }
}

/// Returns `true` if `VK_LBUTTON` is currently depressed in hardware state.
#[inline]
pub fn is_left_button_pressed() -> bool {
    // SAFETY: `GetAsyncKeyState` is a stateless query with no memory safety preconditions.
    unsafe { (GetAsyncKeyState(i32::from(VK_LBUTTON.0)) as u16 & 0x8000) != 0 }
}

/// Captures the current [`StylusSnapshot`] combining atomic tracker state with `VK_LBUTTON`.
#[inline]
pub fn current_snapshot(hover_timeout_ms: u64) -> StylusSnapshot {
    global_tracker().snapshot(hover_timeout_ms, is_left_button_pressed())
}

/// Decodes and dispatches a `WM_INPUT` message (`RIM_TYPEMOUSE` and `RIM_TYPEHID`).
pub fn process_wm_input(lparam: LPARAM) {
    let mut raw_bytes = [0u8; 1024];
    let mut size = raw_bytes.len() as u32;

    // SAFETY: `raw_bytes` provides a valid 1024-byte stack buffer for `GetRawInputData`.
    let res = unsafe {
        GetRawInputData(
            HRAWINPUT(lparam.0 as _),
            RID_INPUT,
            Some(raw_bytes.as_mut_ptr().cast()),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };

    if res == u32::MAX || (size as usize) < size_of::<RAWINPUTHEADER>() {
        return;
    }

    // SAFETY: `GetRawInputData` populated at least `size_of::<RAWINPUTHEADER>()` valid bytes.
    let raw = unsafe { &*(raw_bytes.as_ptr().cast::<RAWINPUT>()) };
    let tracker = global_tracker();

    if raw.header.dwType == RIM_TYPEMOUSE.0 {
        // SAFETY: `raw.data.mouse` is the active union variant when `dwType == RIM_TYPEMOUSE`.
        let mouse = unsafe { raw.data.mouse };
        let is_absolute = (mouse.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0) != 0;
        let extra = mouse.ulExtraInformation as usize;
        let is_pen = is_pen_signature(extra);

        if is_pen || is_absolute {
            let (px, py) = if is_absolute {
                let viewport = current_virtual_screen_bounds();
                DeviceAxisRange::WIN32_ABSOLUTE_MOUSE.map_to_viewport(
                    mouse.lLastX as f32,
                    mouse.lLastY as f32,
                    viewport,
                )
            } else {
                let mut pt = POINT::default();
                // SAFETY: Passes a valid stack pointer to `POINT`.
                unsafe {
                    let _ = GetCursorPos(&mut pt);
                }
                (pt.x as f32, pt.y as f32)
            };

            // SAFETY: Reads the `usButtonFlags` field from the `RAWMOUSE` union.
            let btn_flags = unsafe { u32::from(mouse.Anonymous.Anonymous.usButtonFlags) };
            let btn_down =
                (btn_flags & (RI_MOUSE_LEFT_BUTTON_DOWN | RI_MOUSE_BUTTON_1_DOWN)) != 0;
            let btn_up = (btn_flags & (RI_MOUSE_LEFT_BUTTON_UP | RI_MOUSE_BUTTON_1_UP)) != 0;
            let key_down = is_left_button_pressed();

            let contact_update = if btn_down || key_down {
                Some(true)
            } else if btn_up {
                Some(false)
            } else {
                None
            };

            tracker.mark_pen_active(px, py, contact_update);
        } else if mouse.lLastX != 0 || mouse.lLastY != 0 {
            tracker.mark_mouse_active();
        }
    } else if raw.header.dwType == RIM_TYPEHID.0 {
        // SAFETY: `raw.data.hid` is the active union variant when `dwType == RIM_TYPEHID`.
        let (dw_size, dw_count) = unsafe {
            (
                raw.data.hid.dwSizeHid as usize,
                raw.data.hid.dwCount as usize,
            )
        };
        if dw_size > 0 && dw_count > 0 {
            let report_offset = size_of::<RAWINPUTHEADER>() + size_of::<u32>() * 2;
            if report_offset + dw_size <= raw_bytes.len() {
                let report = &raw_bytes[report_offset..report_offset + dw_size];
                if let Some(digitizer) = parse_digitizer_report(raw.header.hDevice, report) {
                    tracker.mark_pen_active(digitizer.x, digitizer.y, Some(digitizer.is_contact));
                }
            }
        }
    }
}
