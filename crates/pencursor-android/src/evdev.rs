//! Event-driven Linux `evdev` digitizer and mouse monitor with `inotify` hotplug and batch coalescing.
//!
//! Architecture:
//! - Passively opens `/dev/input/event*` with `O_RDONLY | O_NONBLOCK | O_CLOEXEC` (without
//!   `EVIOCGRAB`, preserving Android `EventHub` / `InputReader` direct `touchScreen` pipeline).
//! - Monitors `/dev/input` via `inotify` (`IN_CREATE | IN_DELETE | IN_ATTRIB`) for zero-polling
//!   USB OTG hotplug detection.
//! - Drains all queued `input_event` packets until `EAGAIN` on each `epoll` wakeup, coalescing
//!   intermediate `SYN_REPORT` frames so `SurfaceFlinger` receives at most one `Transaction` per wakeup.

mod device;

use crate::power::apply_usb_power_policy_with_config;
use crate::sys::{
    EPOLLERR, EPOLLHUP, EPOLLIN, O_CLOEXEC, O_NONBLOCK, O_RDONLY, OwnedFd, epoll_add, epoll_del,
    open, read,
};
use device::{
    ABS_PRESSURE, ABS_X, ABS_Y, BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH, DeviceKind, EV_ABS,
    EV_KEY, EV_REL, EV_SYN, LinuxInputEvent, REL_X, REL_Y, SYN_REPORT, classify_evdev_node,
};
use pencursor_core::{StylusStateTracker, UsbPowerConfig, ViewportBounds};
use std::collections::HashMap;
use std::ffi::{CString, c_int, c_void};
use std::fs;

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

#[derive(Debug, Clone, Copy)]
enum CoalescedStylusFrame {
    Active { x: f32, y: f32, is_contact: bool },
    OutOfRange,
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

    /// Scans `/dev/input`, enforces default USB power policy on matching tablets, and registers any new
    /// stylus digitizer or mouse nodes with `epoll_fd`.
    pub fn scan_and_register(&mut self, epoll_fd: c_int) {
        self.scan_and_register_with_config(epoll_fd, &UsbPowerConfig::default());
    }

    /// Scans `/dev/input`, enforces [`UsbPowerConfig`] on matching tablets, and registers any new
    /// stylus digitizer or mouse nodes with `epoll_fd`.
    pub fn scan_and_register_with_config(&mut self, epoll_fd: c_int, usb_power: &UsbPowerConfig) {
        apply_usb_power_policy_with_config(usb_power);

        let target_vid = usb_power.vid_u16();
        let target_pid = usb_power.pid_u16();

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

            let Some(kind) = classify_evdev_node(fd.raw(), target_vid, target_pid) else {
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
                    DeviceKind::StylusDigitizer { axis_range } => match ev.ty {
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
                                let (mapped_x, mapped_y) = axis_range.map_to_viewport(
                                    node.raw_x as f32,
                                    node.raw_y as f32,
                                    viewport,
                                );
                                let is_contact = node.tip_down || node.pressure > 0;
                                coalesced_frame = Some(CoalescedStylusFrame::Active {
                                    x: mapped_x,
                                    y: mapped_y,
                                    is_contact,
                                });
                            }
                        }
                        _ => {}
                    },
                    DeviceKind::RelativeMouse => {
                        if ev.ty == EV_REL
                            && (ev.code == REL_X || ev.code == REL_Y)
                            && ev.value != 0
                        {
                            tracker.mark_mouse_active();
                            state_changed = true;
                        }
                    }
                }
            }
        }

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
