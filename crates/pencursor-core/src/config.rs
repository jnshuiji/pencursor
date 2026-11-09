//! Cursor configuration, Linebender `peniko` color palettes, and USB/OTG runtime power policies.
//!
//! Defines platform-agnostic parameters for stylus overlay geometry, color palettes
//! backed by [`vello_cpu::peniko::Color`], inactivity timeouts, polling intervals,
//! and USB runtime power management policies.

mod kv;

use std::fmt;
use std::str::FromStr;
use std::time::Duration;
use vello_cpu::peniko::Color;

/// Default configuration file path inside the KernelSU module directory on Android.
pub const DEFAULT_ANDROID_CONFIG_PATH: &str = "/data/adb/modules/gaomon_ksu_driver/config.conf";

/// Error returned when parsing a [`CursorStyle`] from a string fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseCursorStyleError;

impl fmt::Display for ParseCursorStyleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid cursor style")
    }
}

impl std::error::Error for ParseCursorStyleError {}

/// Visual presentation style rendered while the stylus is hovering in range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorStyle {
    /// High-contrast crosshair with a transparent center gap.
    Crosshair,
    /// High-contrast circular brush ring with a precision center dot.
    Circle,
    /// Composite overlay combining both crosshair and circular ring.
    #[default]
    Both,
}

impl CursorStyle {
    /// Returns the canonical lowercase configuration identifier for this style.
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Crosshair => "crosshair",
            Self::Circle => "circle",
            Self::Both => "both",
        }
    }

    /// Parses a [`CursorStyle`] from a configuration string (case-insensitive).
    #[inline]
    pub fn parse(value: &str) -> Option<Self> {
        value.parse().ok()
    }
}

impl fmt::Display for CursorStyle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CursorStyle {
    type Err = ParseCursorStyleError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "crosshair" | "cross" => Ok(Self::Crosshair),
            "circle" | "ring" => Ok(Self::Circle),
            "both" | "combined" | "all" => Ok(Self::Both),
            _ => Err(ParseCursorStyleError),
        }
    }
}

/// Color palette for cursor vector primitives using Linebender [`Color`] (`AlphaColor<Srgb>`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorPalette {
    /// Primary foreground stroke and center dot color.
    pub primary: Color,
    /// High-contrast outer shadow/border color for visibility against arbitrary backgrounds.
    pub outline: Color,
    /// Highlight color used when the stylus tip is in physical contact with the surface.
    pub contact: Color,
}

impl Default for CursorPalette {
    fn default() -> Self {
        Self {
            primary: Color::from_rgba8(255, 255, 255, 255),
            outline: Color::from_rgba8(13, 13, 13, 242),
            contact: Color::from_rgba8(0, 230, 255, 255),
        }
    }
}

impl CursorPalette {
    /// Parses a `#RRGGBB` or `#RRGGBBAA` hexadecimal color string into a [`Color`].
    pub fn parse_hex_color(raw: &str) -> Option<Color> {
        let hex = raw.trim().strip_prefix('#').unwrap_or(raw.trim());
        match hex.len() {
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(Color::from_rgba8(r, g, b, 255))
            }
            8 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
                Some(Color::from_rgba8(r, g, b, a))
            }
            _ => None,
        }
    }

    /// Formats a [`Color`] as an uppercase `#RRGGBBAA` hexadecimal string.
    pub fn format_hex_color(color: Color) -> String {
        let rgba = color.to_rgba8();
        format!("#{:02X}{:02X}{:02X}{:02X}", rgba.r, rgba.g, rgba.b, rgba.a)
    }
}

/// USB and OTG runtime power management policy for external EMR drawing tablets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbPowerConfig {
    /// Target USB Vendor ID in 4-digit lowercase hex (default: `"256c"`).
    pub usb_vid: String,
    /// Target USB Product ID in 4-digit lowercase hex (default: `"0064"`).
    pub usb_pid: String,
    /// Whether to disable global `usbcore.autosuspend` (`/sys/module/usbcore/parameters/autosuspend = -1`).
    pub disable_usbcore_autosuspend: bool,
    /// Whether to enforce Oplus/ColorOS OTG switch persistence (`otg_switch = 1` and `persist.sys.oplus.otg_support = 1`).
    pub enforce_oplus_otg_switch: bool,
    /// Whether to force matched USB device `power/control = on` and `autosuspend_delay_ms = -1`.
    pub force_device_power_on: bool,
}

impl Default for UsbPowerConfig {
    fn default() -> Self {
        Self {
            usb_vid: "256c".to_string(),
            usb_pid: "0064".to_string(),
            disable_usbcore_autosuspend: true,
            enforce_oplus_otg_switch: true,
            force_device_power_on: true,
        }
    }
}

impl UsbPowerConfig {
    /// Parses `usb_vid` as a `u16`, falling back to `0x256c` if invalid.
    #[inline]
    pub fn vid_u16(&self) -> u16 {
        let clean = self
            .usb_vid
            .trim()
            .strip_prefix("0x")
            .unwrap_or(self.usb_vid.trim());
        u16::from_str_radix(clean, 16).unwrap_or(0x256c)
    }

    /// Parses `usb_pid` as a `u16`, falling back to `0x0064` if invalid.
    #[inline]
    pub fn pid_u16(&self) -> u16 {
        let clean = self
            .usb_pid
            .trim()
            .strip_prefix("0x")
            .unwrap_or(self.usb_pid.trim());
        u16::from_str_radix(clean, 16).unwrap_or(0x0064)
    }
}

/// Global runtime, rendering, and power management configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Whether the background `pencursor` daemon should remain active.
    pub daemon_enabled: bool,
    /// Whether the hardware `SurfaceFlinger` cursor overlay should be rendered.
    pub overlay_enabled: bool,

    /// Hover cursor visual style.
    pub style: CursorStyle,

    /// Length of each crosshair arm in pixels.
    pub crosshair_size: f64,
    /// Radius of the transparent center gap of the crosshair in pixels.
    pub crosshair_gap: f64,
    /// Stroke thickness of the primary crosshair lines in pixels.
    pub crosshair_thickness: f64,

    /// Radius of the circular cursor ring in pixels.
    pub circle_radius: f64,
    /// Stroke thickness of the circular cursor ring in pixels.
    pub circle_thickness: f64,

    /// Radius of the contracted indicator dot during tip contact in pixels.
    pub contact_dot_radius: f64,

    /// Inactivity threshold in milliseconds before the stylus is considered out of range.
    pub hover_timeout_ms: u64,
    /// Grace period in milliseconds after the last stylus report before physical mouse movement hides the overlay.
    pub mouse_inactivity_timeout_ms: u64,

    /// Frame polling interval in milliseconds while the stylus is active in range.
    pub active_poll_interval_ms: u64,
    /// Frame polling interval in milliseconds while the stylus is idle or out of range.
    pub idle_poll_interval_ms: u64,

    /// Vector rendering color palette.
    pub palette: CursorPalette,

    /// USB and OTG power governor configuration.
    pub usb_power: UsbPowerConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            daemon_enabled: true,
            overlay_enabled: true,
            style: CursorStyle::Both,
            crosshair_size: 10.0,
            crosshair_gap: 3.5,
            crosshair_thickness: 1.5,
            circle_radius: 12.0,
            circle_thickness: 1.5,
            contact_dot_radius: 2.0,
            hover_timeout_ms: 150,
            mouse_inactivity_timeout_ms: 100,
            active_poll_interval_ms: 1,
            idle_poll_interval_ms: 10,
            palette: CursorPalette::default(),
            usb_power: UsbPowerConfig::default(),
        }
    }
}

impl Config {
    /// Returns the target frame sleep duration based on whether the stylus is currently in proximity.
    #[inline]
    pub const fn frame_sleep_duration(&self, in_range: bool) -> Duration {
        if in_range {
            Duration::from_millis(self.active_poll_interval_ms)
        } else {
            Duration::from_millis(self.idle_poll_interval_ms)
        }
    }

    /// Returns `true` if all vector sprite rendering parameters match `other`.
    #[inline]
    pub fn visuals_eq(&self, other: &Self) -> bool {
        self.style == other.style
            && (self.crosshair_size - other.crosshair_size).abs() < 1e-4
            && (self.crosshair_gap - other.crosshair_gap).abs() < 1e-4
            && (self.crosshair_thickness - other.crosshair_thickness).abs() < 1e-4
            && (self.circle_radius - other.circle_radius).abs() < 1e-4
            && (self.circle_thickness - other.circle_thickness).abs() < 1e-4
            && (self.contact_dot_radius - other.contact_dot_radius).abs() < 1e-4
            && self.palette == other.palette
    }

    /// Clamps geometric and timing fields so that `64x64` sprite rasterization and timers stay within safe bounds.
    pub fn clamp_bounds(&mut self) {
        self.crosshair_thickness = self.crosshair_thickness.clamp(0.5, 6.0);
        self.crosshair_gap = self.crosshair_gap.clamp(0.0, 14.0);
        let max_arm = (29.0 - self.crosshair_gap - self.crosshair_thickness * 0.5).max(2.0);
        self.crosshair_size = self.crosshair_size.clamp(2.0, max_arm.min(26.0));

        self.circle_thickness = self.circle_thickness.clamp(0.5, 6.0);
        let max_radius = (29.0 - self.circle_thickness * 0.5).max(2.0);
        self.circle_radius = self.circle_radius.clamp(2.0, max_radius.min(27.0));

        self.contact_dot_radius = self.contact_dot_radius.clamp(0.5, 16.0);
        self.hover_timeout_ms = self.hover_timeout_ms.clamp(20, 5_000);
        self.mouse_inactivity_timeout_ms = self.mouse_inactivity_timeout_ms.clamp(10, 5_000);
        self.active_poll_interval_ms = self.active_poll_interval_ms.clamp(1, 100);
        self.idle_poll_interval_ms = self.idle_poll_interval_ms.clamp(1, 1_000);
    }
}
