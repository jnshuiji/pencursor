//! Zero-dependency `key=value` serialization, deserialization, and file I/O for [`Config`].

use super::{Config, CursorPalette, CursorStyle, DEFAULT_ANDROID_CONFIG_PATH};
use std::convert::Infallible;
use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

impl Config {
    /// Applies `key=value` configuration lines onto `self`, ignoring unknown keys, comments, and malformed values.
    pub fn apply_kv(&mut self, content: &str) {
        for raw_line in content.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((raw_key, raw_val)) = line.split_once('=') else {
                continue;
            };
            let key = raw_key.trim();
            let val = raw_val.trim().trim_matches('"').trim_matches('\'').trim();

            match key {
                "daemon_enabled" => {
                    if let Some(b) = parse_bool(val) {
                        self.daemon_enabled = b;
                    }
                }
                "overlay_enabled" => {
                    if let Some(b) = parse_bool(val) {
                        self.overlay_enabled = b;
                    }
                }
                "style" | "cursor_style" => {
                    if let Some(s) = CursorStyle::parse(val) {
                        self.style = s;
                    }
                }
                "crosshair_size" => {
                    if let Ok(v) = val.parse::<f64>()
                        && v.is_finite()
                    {
                        self.crosshair_size = v;
                    }
                }
                "crosshair_gap" => {
                    if let Ok(v) = val.parse::<f64>()
                        && v.is_finite()
                    {
                        self.crosshair_gap = v;
                    }
                }
                "crosshair_thickness" => {
                    if let Ok(v) = val.parse::<f64>()
                        && v.is_finite()
                    {
                        self.crosshair_thickness = v;
                    }
                }
                "circle_radius" => {
                    if let Ok(v) = val.parse::<f64>()
                        && v.is_finite()
                    {
                        self.circle_radius = v;
                    }
                }
                "circle_thickness" => {
                    if let Ok(v) = val.parse::<f64>()
                        && v.is_finite()
                    {
                        self.circle_thickness = v;
                    }
                }
                "contact_dot_radius" => {
                    if let Ok(v) = val.parse::<f64>()
                        && v.is_finite()
                    {
                        self.contact_dot_radius = v;
                    }
                }
                "hover_timeout_ms" => {
                    if let Ok(v) = val.parse::<u64>() {
                        self.hover_timeout_ms = v;
                    }
                }
                "mouse_inactivity_timeout_ms" => {
                    if let Ok(v) = val.parse::<u64>() {
                        self.mouse_inactivity_timeout_ms = v;
                    }
                }
                "active_poll_interval_ms" => {
                    if let Ok(v) = val.parse::<u64>() {
                        self.active_poll_interval_ms = v;
                    }
                }
                "idle_poll_interval_ms" => {
                    if let Ok(v) = val.parse::<u64>() {
                        self.idle_poll_interval_ms = v;
                    }
                }
                "primary_color" => {
                    if let Some(c) = CursorPalette::parse_hex_color(val) {
                        self.palette.primary = c;
                    }
                }
                "outline_color" => {
                    if let Some(c) = CursorPalette::parse_hex_color(val) {
                        self.palette.outline = c;
                    }
                }
                "contact_color" => {
                    if let Some(c) = CursorPalette::parse_hex_color(val) {
                        self.palette.contact = c;
                    }
                }
                "usb_vid" => {
                    let clean = val.strip_prefix("0x").unwrap_or(val).to_ascii_lowercase();
                    if !clean.is_empty()
                        && clean.len() <= 4
                        && let Ok(num) = u16::from_str_radix(&clean, 16)
                    {
                        self.usb_power.usb_vid = format!("{num:04x}");
                    }
                }
                "usb_pid" => {
                    let clean = val.strip_prefix("0x").unwrap_or(val).to_ascii_lowercase();
                    if !clean.is_empty()
                        && clean.len() <= 4
                        && let Ok(num) = u16::from_str_radix(&clean, 16)
                    {
                        self.usb_power.usb_pid = format!("{num:04x}");
                    }
                }
                "disable_usbcore_autosuspend" => {
                    if let Some(b) = parse_bool(val) {
                        self.usb_power.disable_usbcore_autosuspend = b;
                    }
                }
                "enforce_oplus_otg_switch" => {
                    if let Some(b) = parse_bool(val) {
                        self.usb_power.enforce_oplus_otg_switch = b;
                    }
                }
                "force_device_power_on" => {
                    if let Some(b) = parse_bool(val) {
                        self.usb_power.force_device_power_on = b;
                    }
                }
                _ => {}
            }
        }
        self.clamp_bounds();
    }

    /// Parses a [`Config`] from a `key=value` configuration text, falling back to [`Config::default`] for omitted keys.
    #[inline]
    pub fn parse_kv(content: &str) -> Self {
        let mut cfg = Self::default();
        cfg.apply_kv(content);
        cfg
    }

    /// Serializes this [`Config`] into a formatted `key=value` configuration string.
    #[inline]
    pub fn to_kv_string(&self) -> String {
        self.to_string()
    }

    /// Loads [`Config`] from a file path on disk.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        Ok(Self::parse_kv(&text))
    }

    /// Loads [`Config`] from `path` if readable, or returns [`Config::default`].
    pub fn load_or_default<P: AsRef<Path>>(path: P) -> Self {
        Self::load_from_file(path).unwrap_or_default()
    }

    /// Saves [`Config`] to a file path on disk.
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        fs::write(path, self.to_string())
    }

    /// Resolves the runtime configuration file path from CLI arguments (`--config <path>` / `-c <path>`),
    /// environment variable (`PENCURSOR_CONFIG`), or [`DEFAULT_ANDROID_CONFIG_PATH`].
    pub fn resolve_config_path() -> PathBuf {
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            if (arg == "--config" || arg == "-c")
                && let Some(p) = args.next()
            {
                return PathBuf::from(p);
            } else if let Some(p) = arg.strip_prefix("--config=") {
                return PathBuf::from(p);
            }
        }
        if let Ok(env_path) = env::var("PENCURSOR_CONFIG")
            && !env_path.trim().is_empty()
        {
            return PathBuf::from(env_path);
        }
        PathBuf::from(DEFAULT_ANDROID_CONFIG_PATH)
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "# Gaomon Tablet Driver & PenCursor Configuration")?;
        writeln!(f)?;
        writeln!(f, "daemon_enabled={}", self.daemon_enabled)?;
        writeln!(f, "overlay_enabled={}", self.overlay_enabled)?;
        writeln!(f, "style={}", self.style)?;
        writeln!(f)?;
        writeln!(f, "crosshair_size={:.2}", self.crosshair_size)?;
        writeln!(f, "crosshair_gap={:.2}", self.crosshair_gap)?;
        writeln!(f, "crosshair_thickness={:.2}", self.crosshair_thickness)?;
        writeln!(f, "circle_radius={:.2}", self.circle_radius)?;
        writeln!(f, "circle_thickness={:.2}", self.circle_thickness)?;
        writeln!(f, "contact_dot_radius={:.2}", self.contact_dot_radius)?;
        writeln!(f)?;
        writeln!(
            f,
            "primary_color={}",
            CursorPalette::format_hex_color(self.palette.primary)
        )?;
        writeln!(
            f,
            "outline_color={}",
            CursorPalette::format_hex_color(self.palette.outline)
        )?;
        writeln!(
            f,
            "contact_color={}",
            CursorPalette::format_hex_color(self.palette.contact)
        )?;
        writeln!(f)?;
        writeln!(f, "hover_timeout_ms={}", self.hover_timeout_ms)?;
        writeln!(
            f,
            "mouse_inactivity_timeout_ms={}",
            self.mouse_inactivity_timeout_ms
        )?;
        writeln!(
            f,
            "active_poll_interval_ms={}",
            self.active_poll_interval_ms
        )?;
        writeln!(f, "idle_poll_interval_ms={}", self.idle_poll_interval_ms)?;
        writeln!(f)?;
        writeln!(f, "usb_vid={}", self.usb_power.usb_vid)?;
        writeln!(f, "usb_pid={}", self.usb_power.usb_pid)?;
        writeln!(
            f,
            "disable_usbcore_autosuspend={}",
            self.usb_power.disable_usbcore_autosuspend
        )?;
        writeln!(
            f,
            "enforce_oplus_otg_switch={}",
            self.usb_power.enforce_oplus_otg_switch
        )?;
        writeln!(
            f,
            "force_device_power_on={}",
            self.usb_power.force_device_power_on
        )
    }
}

impl FromStr for Config {
    type Err = Infallible;

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse_kv(s))
    }
}

#[inline]
fn parse_bool(val: &str) -> Option<bool> {
    match val.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}
