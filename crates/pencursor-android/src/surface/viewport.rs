//! Display viewport resolution detection via DRM sysfs and Android WindowManager.

use pencursor_core::ViewportBounds;
use std::fs;
use std::process::Command;

/// Detects the native physical display viewport bounds (e.g., `2400x3392` Portrait panel on OnePlus Pad 3 Pro).
pub fn detect_native_display_bounds() -> ViewportBounds {
    // 1. Prefer DRM DSI connector mode (reports native panel Portrait resolution `2400x3392`).
    if let Ok(entries) = fs::read_dir("/sys/class/drm") {
        for e in entries.flatten() {
            let name = e.file_name();
            let s = name.to_string_lossy();
            if s.contains("DSI") || s.contains("dsi") || s.contains("panel") {
                let mode_path = e.path().join("modes");
                if let Ok(txt) = fs::read_to_string(mode_path) {
                    for line in txt.lines() {
                        let clean = line.trim().split(['-', '@', ' ']).next().unwrap_or("");
                        let mut it = clean.split('x');
                        if let (Some(a), Some(b)) = (it.next(), it.next())
                            && let (Ok(w), Ok(h)) = (a.parse::<u32>(), b.parse::<u32>())
                            && w >= 480
                            && h >= 480
                        {
                            return ViewportBounds::new(0.0, 0.0, w as f32, h as f32);
                        }
                    }
                }
            }
        }
    }

    // 2. Fallback to `/system/bin/wm size`.
    if let Ok(output) = Command::new("/system/bin/wm").arg("size").output() {
        let txt = String::from_utf8_lossy(&output.stdout);
        for line in txt.lines() {
            if let Some(pos) = line.find(':') {
                let val_part = line[pos + 1..].trim();
                let mut it = val_part.split('x');
                if let (Some(a), Some(b)) = (it.next(), it.next())
                    && let (Ok(w), Ok(h)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>())
                    && w >= 480
                    && h >= 480
                {
                    return ViewportBounds::new(0.0, 0.0, w as f32, h as f32);
                }
            }
        }
    }

    // 3. Default baseline for OnePlus Pad 3 Pro (2400x3392 native portrait frame).
    ViewportBounds::new(0.0, 0.0, 2400.0, 3392.0)
}
