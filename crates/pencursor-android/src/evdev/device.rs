//! Linux `evdev` kernel data structures, `ioctl` bitmask probes, and device node classification.

use crate::sys::ioctl;
use pencursor_core::DeviceAxisRange;
use std::ffi::{c_int, c_ulong};

pub(super) const GAOMON_VID_NUM: u16 = 0x256c;
pub(super) const GAOMON_PID_NUM: u16 = 0x0064;

pub(super) const EV_SYN: u16 = 0x00;
pub(super) const EV_KEY: u16 = 0x01;
pub(super) const EV_REL: u16 = 0x02;
pub(super) const EV_ABS: u16 = 0x03;

pub(super) const SYN_REPORT: u16 = 0x00;

pub(super) const REL_X: u16 = 0x00;
pub(super) const REL_Y: u16 = 0x01;

pub(super) const ABS_X: u16 = 0x00;
pub(super) const ABS_Y: u16 = 0x01;
pub(super) const ABS_PRESSURE: u16 = 0x18;

pub(super) const BTN_LEFT: u16 = 0x110;
pub(super) const BTN_TOOL_PEN: u16 = 0x140;
pub(super) const BTN_TOOL_RUBBER: u16 = 0x141;
pub(super) const BTN_TOUCH: u16 = 0x14a;
pub(super) const BTN_STYLUS: u16 = 0x14b;

/// `_IOR('E', 0x02, struct input_id)` (`sizeof(input_id) == 8`).
const EVIOCGID: c_ulong = (2u64 << 30) | (8u64 << 16) | ((b'E' as u64) << 8) | 0x02;

/// `_IOR('E', 0x20 + ev, len)`.
#[inline(always)]
const fn eviocgbit(ev: u32, len: usize) -> c_ulong {
    (2u64 << 30) | ((len as u64) << 16) | ((b'E' as u64) << 8) | ((0x20 + ev) as u64)
}

/// `_IOR('E', 0x40 + abs, struct input_absinfo)` (`sizeof(input_absinfo) == 24`).
#[inline(always)]
const fn eviocgabs(abs: u32) -> c_ulong {
    (2u64 << 30) | (24u64 << 16) | ((b'E' as u64) << 8) | ((0x40 + abs) as u64)
}

#[inline]
fn test_bit(bit: u16, bytes: &[u8]) -> bool {
    let idx = (bit as usize) / 8;
    let shift = (bit as usize) % 8;
    idx < bytes.len() && (bytes[idx] & (1u8 << shift)) != 0
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct LinuxInputEvent {
    pub(super) tv_sec: i64,
    pub(super) tv_usec: i64,
    pub(super) ty: u16,
    pub(super) code: u16,
    pub(super) value: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct InputAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum DeviceKind {
    StylusDigitizer { axis_range: DeviceAxisRange },
    RelativeMouse,
}

/// Inspects an open `evdev` file descriptor via `ioctl` to determine whether it is a stylus
/// digitizer or a physical relative mouse.
pub(super) fn classify_evdev_node(
    fd: c_int,
    target_vid: u16,
    target_pid: u16,
) -> Option<DeviceKind> {
    let mut id = InputId::default();
    // SAFETY: Queries `EVIOCGID` into stack-allocated `InputId`.
    let has_id = unsafe { ioctl(fd, EVIOCGID, &raw mut id) == 0 };

    let mut key_bits = [0u8; 96];
    // SAFETY: Queries `EV_KEY` capability bitmask into `key_bits`.
    let _ = unsafe {
        ioctl(
            fd,
            eviocgbit(EV_KEY as u32, key_bits.len()),
            key_bits.as_mut_ptr(),
        )
    };

    let mut abs_x = InputAbsInfo::default();
    let mut abs_y = InputAbsInfo::default();
    // SAFETY: Queries `ABS_X` and `ABS_Y` axis descriptors into `InputAbsInfo`.
    let has_abs_x = unsafe { ioctl(fd, eviocgabs(ABS_X as u32), &raw mut abs_x) == 0 };
    // SAFETY: Queries `ABS_Y` axis descriptor into `InputAbsInfo`.
    let has_abs_y = unsafe { ioctl(fd, eviocgabs(ABS_Y as u32), &raw mut abs_y) == 0 };

    let valid_abs_xy =
        has_abs_x && has_abs_y && abs_x.maximum > abs_x.minimum && abs_y.maximum > abs_y.minimum;

    let is_target_tablet = has_id
        && ((id.vendor == GAOMON_VID_NUM && id.product == GAOMON_PID_NUM)
            || (id.vendor == target_vid && id.product == target_pid));
    let has_pen_tool = test_bit(BTN_TOOL_PEN, &key_bits) || test_bit(BTN_STYLUS, &key_bits);

    if valid_abs_xy && (is_target_tablet || has_pen_tool) {
        return Some(DeviceKind::StylusDigitizer {
            axis_range: DeviceAxisRange::new(
                abs_x.minimum as f32,
                abs_x.maximum as f32,
                abs_y.minimum as f32,
                abs_y.maximum as f32,
            ),
        });
    }

    let mut rel_bits = [0u8; 8];
    // SAFETY: Queries `EV_REL` capability bitmask into `rel_bits`.
    let has_rel = unsafe {
        ioctl(
            fd,
            eviocgbit(EV_REL as u32, rel_bits.len()),
            rel_bits.as_mut_ptr(),
        ) >= 0
    };

    if has_rel
        && !valid_abs_xy
        && test_bit(REL_X, &rel_bits)
        && test_bit(REL_Y, &rel_bits)
        && test_bit(BTN_LEFT, &key_bits)
    {
        return Some(DeviceKind::RelativeMouse);
    }

    None
}
