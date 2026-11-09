//! Event-driven sysfs USB runtime power and OTG governor for passive EMR digitizers.
//!
//! Enforces `power/control = on`, `power/autosuspend_delay_ms = -1`, `power/wakeup = enabled`,
//! and OTG VBUS persistence on daemon startup and whenever `/dev/input` hotplug events occur.

use std::fs;
use std::path::Path;

/// Default Huion / Gaomon USB Vendor ID (`0x256c`).
pub const GAOMON_USB_VID: &str = "256c";

/// Default Gaomon Tablet_M8 USB Product ID (`0x0064`).
pub const GAOMON_USB_PID: &str = "0064";

/// Applies global USB autosuspend overrides, OEM OTG switch state, and per-device runtime power
/// policies for matching USB digitizer peripherals.
pub fn apply_usb_power_policy(target_vid: &str, target_pid: &str) {
    // 1. Disable global usbcore autosuspend timer.
    write_sysfs_if_needed("/sys/module/usbcore/parameters/autosuspend", "-1");

    // 2. Enforce Oplus / ColorOS OTG VBUS switch persistence if present.
    write_sysfs_if_needed("/sys/devices/virtual/oplus_chg/usb/otg_switch", "1");

    // 3. Configure matching USB devices under /sys/bus/usb/devices/*.
    let Ok(entries) = fs::read_dir("/sys/bus/usb/devices") else {
        return;
    };

    for entry in entries.flatten() {
        let dev_path = entry.path();
        let vid_path = dev_path.join("idVendor");
        let pid_path = dev_path.join("idProduct");

        let Ok(vid) = fs::read_to_string(&vid_path) else {
            continue;
        };
        let Ok(pid) = fs::read_to_string(&pid_path) else {
            continue;
        };

        if vid.trim().eq_ignore_ascii_case(target_vid)
            && pid.trim().eq_ignore_ascii_case(target_pid)
        {
            let power_dir = dev_path.join("power");
            write_sysfs_if_needed(power_dir.join("control"), "on");
            write_sysfs_if_needed(power_dir.join("autosuspend_delay_ms"), "-1");
            write_sysfs_if_needed(power_dir.join("wakeup"), "enabled");
        }
    }
}

fn write_sysfs_if_needed<P: AsRef<Path>>(path: P, target_val: &str) {
    let path = path.as_ref();
    if let Ok(current) = fs::read_to_string(path)
        && current.trim() == target_val
    {
        return;
    }
    let _ = fs::write(path, target_val);
}
