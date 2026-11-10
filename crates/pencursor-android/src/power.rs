//! Event-driven sysfs USB runtime power and OTG governor for passive EMR digitizers.
//!
//! Enforces `power/control = on`, `power/autosuspend_delay_ms = -1`, `power/wakeup = enabled`,
//! and OTG VBUS persistence on daemon startup, hotplug events, and WebUI configuration updates.

use pencursor_core::UsbPowerConfig;
use std::fs;
use std::path::Path;

/// Applies USB and OTG runtime power policies according to [`UsbPowerConfig`].
pub fn apply_usb_power_policy_with_config(cfg: &UsbPowerConfig) {
    // 1. Configure global usbcore autosuspend timer.
    let autosuspend_val = if cfg.disable_usbcore_autosuspend {
        "-1"
    } else {
        "2"
    };
    write_sysfs_if_needed(
        "/sys/module/usbcore/parameters/autosuspend",
        autosuspend_val,
    );

    // 2. Enforce Oplus / ColorOS OTG VBUS switch persistence if enabled and present.
    if cfg.enforce_oplus_otg_switch {
        write_sysfs_if_needed("/sys/devices/virtual/oplus_chg/usb/otg_switch", "1");
    }

    // 3. Configure matching USB devices under /sys/bus/usb/devices/*.
    let Ok(entries) = fs::read_dir("/sys/bus/usb/devices") else {
        return;
    };

    let target_vid = cfg
        .usb_vid
        .trim()
        .strip_prefix("0x")
        .unwrap_or(cfg.usb_vid.trim());
    let target_pid = cfg
        .usb_pid
        .trim()
        .strip_prefix("0x")
        .unwrap_or(cfg.usb_pid.trim());

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
            if cfg.force_device_power_on {
                write_sysfs_if_needed(power_dir.join("control"), "on");
                write_sysfs_if_needed(power_dir.join("autosuspend_delay_ms"), "-1");
                write_sysfs_if_needed(power_dir.join("wakeup"), "enabled");
            } else {
                write_sysfs_if_needed(power_dir.join("control"), "auto");
                write_sysfs_if_needed(power_dir.join("autosuspend_delay_ms"), "60000");
            }
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
