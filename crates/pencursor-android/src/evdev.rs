//! Event-driven Linux `evdev` digitizer and mouse monitor with `inotify` hotplug and batch coalescing.
//!
//! Architecture:
//! - Passively opens `/dev/input/event*` with `O_RDONLY | O_NONBLOCK | O_CLOEXEC` (without
//!   `EVIOCGRAB`, preserving Android `EventHub` / `InputReader` direct `touchScreen` pipeline).
//! - Monitors `/dev/input` via `inotify` (`IN_CREATE | IN_DELETE | IN_ATTRIB`) for zero-polling
//!   USB OTG hotplug detection.
//! - Drains all queued `input_event` packets until `EAGAIN` on each `epoll` wakeup, coalescing
//!   intermediate `SYN_REPORT` frames so `SurfaceFlinger` receives at most one `Transaction` per wakeup.

use crate::power::{GAOMON_USB_PID, GAOMON_USB_VID, apply_usb_power_policy};
use crate::sys::{
    EPOLLERR, EPOLLHUP, EPOLLIN, O_CLOEXEC, O_NONBLOCK, O_RDONLY, OwnedFd, epoll_add, epoll_del,
    ioctl, open, read,
};
use pencursor_core::{DeviceAxisRange, StylusStateTracker, ViewportBounds};
use std::collections::HashMap;
use std::ffi::{CString, c_int, c_ulong, c_void};
use std::fs;

const GAOMON_VID_NUM: u16 = 0x256c;
const GAOMON_PID_NUM: u16 = 0x0064;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

const SYN_REPORT: u16 = 0x00;

const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_PRESSURE: u16 = 0x18;

const BTN_LEFT: u16 = 0x110;
const BTN_TOOL_PEN: u16 = 0x140;
const BTN_TOOL_RUBBER: u16 = 0x141;
const BTN_TOUCH: u16 = 0x14a;
const BTN_STYLUS: u16 = 0x14b;

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
struct LinuxInputEvent {
    tv_sec: i64,
    tv_usec: i64,
    ty: u16,
    code: u16,
    value: i32,
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
enum DeviceKind {
    StylusDigitizer { axis_range: DeviceAxisRange },
    RelativeMouse,
}

#[derive(Debug)]
struct EvdevNode {
    path: String,
    fd: OwnedFd,
    kind: DeviceKind,
    raw_x: i32,
    raw_y: i32,
    pressure: i32,
    tip_down: bool,
    in_proximity: bool,
    pending_activity: bool,
    pending_out_of_range: bool,
}

/// Manages active `/dev/input/event*` file descriptors registered with the daemon's `epoll` loop.
#[derive(Debug, Default)]
pub struct EvdevManager {
    /// Maps epoll tag (`fd as u64`) to its open [`EvdevNode`].
    nodes: HashMap<u64, EvdevNode>,
}

impl EvdevManager {
    /// Creates an empty [`EvdevManager`].
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    /// Scans `/dev/input`, enforces USB power policy on matching tablets, and registers any new
    /// stylus digitizer or mouse nodes with `epoll_fd`.
    pub fn scan_and_register(&mut self, epoll_fd: c_int) {
        apply_usb_power_policy(GAOMON_USB_VID, GAOMON_USB_PID);

        let Ok(dir) = fs::read_dir("/dev/input") else {
            return;
        };

        for entry in dir.flatten() {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if !name.starts_with("event") {
                continue;
            }

            let path = format!("/dev/input/{name}");
            if self.nodes.values().any(|n| n.path == path) {
                continue;
            }

            let Ok(c_path) = CString::new(path.as_str()) else {
                continue;
            };

            // SAFETY: Opens the evdev character device read-only and non-blocking without EVIOCGRAB.
            let raw_fd = unsafe { open(c_path.as_ptr(), O_RDONLY | O_NONBLOCK | O_CLOEXEC) };
            if raw_fd < 0 {
                continue;
            }
            // SAFETY: `raw_fd` is a valid newly opened file descriptor.
            let fd = unsafe { OwnedFd::from_raw(raw_fd) };

            let Some(kind) = classify_evdev_node(fd.raw()) else {
                continue;
            };

            let tag = fd.raw() as u64;
            if epoll_add(epoll_fd, fd.raw(), EPOLLIN | EPOLLHUP | EPOLLERR, tag).is_ok() {
                self.nodes.insert(
                    tag,
                    EvdevNode {
                        path,
                        fd,
                        kind,
                        raw_x: 0,
                        raw_y: 0,
                        pressure: 0,
                        tip_down: false,
                        in_proximity: false,
                        pending_activity: false,
                        pending_out_of_range: false,
                    },
                );
            }
        }
    }

    /// Removes and closes a disconnected `evdev` node identified by `tag`.
    pub fn remove_by_tag(&mut self, epoll_fd: c_int, tag: u64) {
        if let Some(node) = self.nodes.remove(&tag) {
            epoll_del(epoll_fd, node.fd.raw());
        }
    }

    /// Drains all queued `input_event` packets from the device identified by `tag` until `EAGAIN`,
    /// coalescing intermediate `SYN_REPORT` frames and updating `tracker`.
    ///
    /// Returns `true` if stylus or mouse state changed during this batch.
    pub fn drain_device_events(
        &mut self,
        epoll_fd: c_int,
        tag: u64,
        events_mask: u32,
        viewport: ViewportBounds,
        tracker: &StylusStateTracker,
    ) -> bool {
        if (events_mask & (EPOLLHUP | EPOLLERR)) != 0 {
            self.remove_by_tag(epoll_fd, tag);
            tracker.mark_out_of_range();
            return true;
        }

        let Some(node) = self.nodes.get_mut(&tag) else {
            return false;
        };

        let mut ev_buf = [LinuxInputEvent::default(); 64];
        let elem_sz = size_of::<LinuxInputEvent>();
        let mut state_changed = false;
        let mut coalesced_frame: Option<CoalescedStylusFrame> = None;
        let mut disconnect = false;

        loop {
            // SAFETY: Reads an integral number of `LinuxInputEvent` structs into `ev_buf`.
            let n_bytes = unsafe {
                read(
                    node.fd.raw(),
                    ev_buf.as_mut_ptr().cast::<c_void>(),
                    ev_buf.len() * elem_sz,
                )
            };

            if n_bytes < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() != std::io::ErrorKind::WouldBlock {
                    disconnect = true;
                }
                break;
            }
            if (n_bytes as usize) < elem_sz {
                break;
            }

            let count = (n_bytes as usize) / elem_sz;
            for ev in &ev_buf[..count] {
                match node.kind {
                    DeviceKind::StylusDigitizer { axis_range } => {
                        match ev.ty {
                            EV_ABS => match ev.code {
                                ABS_X => {
                                    node.raw_x = ev.value;
                                    node.pending_activity = true;
                                }
                                ABS_Y => {
                                    node.raw_y = ev.value;
                                    node.pending_activity = true;
                                }
                                ABS_PRESSURE => {
                                    node.pressure = ev.value;
                                    node.pending_activity = true;
                                }
                                _ => {}
                            },
                            EV_KEY => match ev.code {
                                BTN_TOUCH => {
                                    node.tip_down = ev.value != 0;
                                    node.pending_activity = true;
                                }
                                BTN_TOOL_PEN | BTN_TOOL_RUBBER => {
                                    node.in_proximity = ev.value != 0;
                                    if node.in_proximity {
                                        node.pending_activity = true;
                                        node.pending_out_of_range = false;
                                    } else {
                                        node.pending_out_of_range = true;
                                        node.pending_activity = false;
                                        node.tip_down = false;
                                        node.pressure = 0;
                                    }
                                }
                                _ => {}
                            },
                            EV_SYN if ev.code == SYN_REPORT => {
                                if node.pending_out_of_range {
                                    node.pending_out_of_range = false;
                                    coalesced_frame = Some(CoalescedStylusFrame::OutOfRange);
                                } else if node.pending_activity {
                                    node.pending_activity = false;
                                    let mapped = axis_range.map_to_viewport(
                                        node.raw_x as f32,
                                        node.raw_y as f32,
                                        viewport,
                                    );
                                    let is_contact = node.tip_down || node.pressure > 0;
                                    coalesced_frame = Some(CoalescedStylusFrame::Active {
                                        x: mapped.x,
                                        y: mapped.y,
                                        is_contact,
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                    DeviceKind::RelativeMouse => {
                        if ev.ty == EV_REL && (ev.code == REL_X || ev.code == REL_Y) && ev.value != 0
                        {
                            tracker.mark_mouse_active();
                            state_changed = true;
                        }
                    }
                }
            }
        }

        // Apply only the final coalesced SYN_REPORT state from this epoll wakeup batch.
        if let Some(frame) = coalesced_frame {
            match frame {
                CoalescedStylusFrame::Active { x, y, is_contact } => {
                    tracker.mark_pen_active(x, y, Some(is_contact));
                }
                CoalescedStylusFrame::OutOfRange => {
                    tracker.mark_out_of_range();
                }
            }
            state_changed = true;
        }

        if disconnect {
            self.remove_by_tag(epoll_fd, tag);
            tracker.mark_out_of_range();
            state_changed = true;
        }

        state_changed
    }
}

#[derive(Debug, Clone, Copy)]
enum CoalescedStylusFrame {
    Active { x: f32, y: f32, is_contact: bool },
    OutOfRange,
}

/// Inspects an open `evdev` file descriptor via `ioctl` to determine whether it is a stylus
/// digitizer or a physical relative mouse.
fn classify_evdev_node(fd: c_int) -> Option<DeviceKind> {
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

    let is_gaomon = has_id && id.vendor == GAOMON_VID_NUM && id.product == GAOMON_PID_NUM;
    let has_pen_tool = test_bit(BTN_TOOL_PEN, &key_bits) || test_bit(BTN_STYLUS, &key_bits);

    if valid_abs_xy && (is_gaomon || has_pen_tool) {
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
