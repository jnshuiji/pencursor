//! Lock-free atomic stylus state machine and cross-thread snapshot synchronization.
//!
//! Packs 2D `f32` coordinates into a single `AtomicU64` (upper 32 bits for `X`, lower 32 bits
//! for `Y`) to guarantee tear-free concurrent reads and writes across input and rendering threads,
//! while managing hover timeout expiration and physical mouse preemption.

use crate::config::Config;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// Instantaneous state snapshot of the stylus consumed by the rendering pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StylusSnapshot {
    /// Global X coordinate in virtual screen space (pixels).
    pub x: f32,
    /// Global Y coordinate in virtual screen space (pixels).
    pub y: f32,
    /// Whether the stylus transducer is currently in sensing proximity (hovering or in contact).
    pub in_range: bool,
    /// Whether the stylus tip is physically pressing against the digitizer surface.
    pub is_contact: bool,
}

/// Normalized cross-platform input event fed into [`StylusStateTracker`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    /// Stylus position update and optional contact state transition.
    StylusActivity {
        x: f32,
        y: f32,
        /// `Some(true)` on tip down, `Some(false)` on tip up/hover, `None` to preserve prior contact state.
        is_contact: Option<bool>,
    },
    /// Explicit notification that the stylus left sensing proximity (e.g., `BTN_TOOL_PEN = 0`).
    StylusOutOfRange,
    /// Physical mouse movement detected (triggers stylus overlay preemption after timeout).
    MouseActivity,
}

/// Packs a pair of `f32` coordinates bit-exact into a single `u64` word.
#[inline]
pub const fn pack_coords(x: f32, y: f32) -> u64 {
    ((x.to_bits() as u64) << 32) | (y.to_bits() as u64)
}

/// Unpacks a 64-bit word produced by [`pack_coords`] back into `(x, y)` `f32` coordinates.
#[inline]
pub const fn unpack_coords(packed: u64) -> (f32, f32) {
    let x = f32::from_bits((packed >> 32) as u32);
    let y = f32::from_bits(packed as u32);
    (x, y)
}

/// Lock-free atomic state tracker for stylus coordinates, proximity, and contact state.
#[derive(Debug)]
pub struct StylusStateTracker {
    epoch: Instant,
    packed_coords: AtomicU64,
    in_range: AtomicBool,
    is_contact: AtomicBool,
    last_update_ms: AtomicU64,
    hover_timeout_ms: AtomicU64,
    mouse_inactivity_timeout_ms: AtomicU64,
}

impl Default for StylusStateTracker {
    fn default() -> Self {
        let cfg = Config::default();
        Self::new(cfg.hover_timeout_ms, cfg.mouse_inactivity_timeout_ms)
    }
}

impl StylusStateTracker {
    /// Creates a new state tracker initialized with the specified timeout thresholds.
    pub fn new(hover_timeout_ms: u64, mouse_inactivity_timeout_ms: u64) -> Self {
        Self {
            epoch: Instant::now(),
            packed_coords: AtomicU64::new(pack_coords(0.0, 0.0)),
            in_range: AtomicBool::new(false),
            is_contact: AtomicBool::new(false),
            last_update_ms: AtomicU64::new(0),
            hover_timeout_ms: AtomicU64::new(hover_timeout_ms),
            mouse_inactivity_timeout_ms: AtomicU64::new(mouse_inactivity_timeout_ms),
        }
    }

    /// Synchronizes timeout thresholds from the provided [`Config`].
    pub fn configure_from(&self, config: &Config) {
        self.hover_timeout_ms
            .store(config.hover_timeout_ms, Ordering::Relaxed);
        self.mouse_inactivity_timeout_ms
            .store(config.mouse_inactivity_timeout_ms, Ordering::Relaxed);
    }

    /// Returns monotonic milliseconds elapsed since tracker initialization.
    #[inline]
    pub fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// Processes a normalized [`InputEvent`].
    #[inline]
    pub fn handle_event(&self, event: InputEvent) {
        match event {
            InputEvent::StylusActivity { x, y, is_contact } => {
                self.mark_pen_active(x, y, is_contact);
            }
            InputEvent::StylusOutOfRange => {
                self.mark_out_of_range();
            }
            InputEvent::MouseActivity => {
                self.mark_mouse_active();
            }
        }
    }

    /// Records stylus activity at `(x, y)` and optionally updates tip contact state.
    #[inline]
    pub fn mark_pen_active(&self, x: f32, y: f32, is_contact: Option<bool>) {
        self.mark_pen_active_at(x, y, is_contact, self.now_ms());
    }

    /// Records stylus activity at an explicit monotonic timestamp `now_ms`.
    #[inline]
    pub fn mark_pen_active_at(&self, x: f32, y: f32, is_contact: Option<bool>, now_ms: u64) {
        self.packed_coords
            .store(pack_coords(x, y), Ordering::Relaxed);
        if let Some(contact) = is_contact {
            self.is_contact.store(contact, Ordering::Relaxed);
        }
        self.last_update_ms.store(now_ms, Ordering::Release);
        self.in_range.store(true, Ordering::Release);
    }

    /// Explicitly marks the stylus as out of sensing range and clears contact state.
    #[inline]
    pub fn mark_out_of_range(&self) {
        self.in_range.store(false, Ordering::Release);
        self.is_contact.store(false, Ordering::Relaxed);
    }

    /// Records physical mouse movement using the configured `mouse_inactivity_timeout_ms`.
    #[inline]
    pub fn mark_mouse_active(&self) {
        let timeout_ms = self.mouse_inactivity_timeout_ms.load(Ordering::Relaxed);
        self.mark_mouse_active_with_timeout_at(timeout_ms, self.now_ms());
    }

    /// Records physical mouse movement using an explicit `timeout_ms` threshold.
    #[inline]
    pub fn mark_mouse_active_with_timeout(&self, timeout_ms: u64) {
        self.mark_mouse_active_with_timeout_at(timeout_ms, self.now_ms());
    }

    /// Evaluates physical mouse preemption against the last stylus timestamp at `now_ms`.
    #[inline]
    pub fn mark_mouse_active_with_timeout_at(&self, timeout_ms: u64, now_ms: u64) {
        let last_pen = self.last_update_ms.load(Ordering::Acquire);
        if now_ms.saturating_sub(last_pen) > timeout_ms {
            self.mark_out_of_range();
        }
    }

    /// Returns `true` if the stylus is currently within proximity and has not timed out.
    #[inline]
    pub fn is_pen_in_range(&self) -> bool {
        let timeout_ms = self.hover_timeout_ms.load(Ordering::Relaxed);
        self.is_pen_in_range_at(timeout_ms, self.now_ms())
    }

    /// Evaluates whether the stylus is in range at `now_ms` given `timeout_ms`.
    #[inline]
    pub fn is_pen_in_range_at(&self, timeout_ms: u64, now_ms: u64) -> bool {
        let last = self.last_update_ms.load(Ordering::Acquire);
        now_ms.saturating_sub(last) < timeout_ms && self.in_range.load(Ordering::Acquire)
    }

    /// Captures an atomic [`StylusSnapshot`] for the current frame.
    ///
    /// `external_contact_pressed` allows platform backends to OR in asynchronous button state
    /// (such as Win32 `VK_LBUTTON`).
    #[inline]
    pub fn snapshot(&self, timeout_ms: u64, external_contact_pressed: bool) -> StylusSnapshot {
        self.snapshot_at(timeout_ms, self.now_ms(), external_contact_pressed)
    }

    /// Captures an atomic [`StylusSnapshot`] evaluated at explicit timestamp `now_ms`.
    #[inline]
    pub fn snapshot_at(
        &self,
        timeout_ms: u64,
        now_ms: u64,
        external_contact_pressed: bool,
    ) -> StylusSnapshot {
        let in_range = self.is_pen_in_range_at(timeout_ms, now_ms);
        let (x, y) = unpack_coords(self.packed_coords.load(Ordering::Relaxed));

        let is_contact = if in_range {
            external_contact_pressed || self.is_contact.load(Ordering::Relaxed)
        } else {
            false
        };

        StylusSnapshot {
            x,
            y,
            in_range,
            is_contact,
        }
    }
}

/// Process-wide [`StylusStateTracker`] instance for OS C-ABI callbacks (e.g., Win32 `WH_MOUSE_LL`).
static GLOBAL_TRACKER: LazyLock<StylusStateTracker> = LazyLock::new(StylusStateTracker::default);

/// Returns a reference to the process-wide [`StylusStateTracker`].
#[inline]
pub fn global_tracker() -> &'static StylusStateTracker {
    &GLOBAL_TRACKER
}
