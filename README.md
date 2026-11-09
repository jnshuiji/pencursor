# PenCursor

A low-latency, hardware-accelerated stylus cursor overlay service engineered for external drawing tablets and pen digitizers. Designed with a modular cross-platform Rust 2024 workspace architecture supporting Windows (`Win32 RawInput` + `DirectComposition`) and Android (`KernelSU Daemon` + `evdev` + `SurfaceFlinger`).

---

## Architecture Overview

PenCursor follows a Ports & Adapters (Hexagonal) workspace structure that strictly separates platform-agnostic domain logic from operating-system-specific input and graphics backends:

```text
pencursor/
├── Cargo.toml                  # Workspace manifest (Edition 2024, Resolver v3, LTO Release profile)
└── crates/
    ├── pencursor-core/         # Platform-agnostic domain model, lock-free state engine, and geometry
    ├── pencursor-windows/      # Windows backend (RawInput HID/Mouse, WH_MOUSE_LL, D3D11/DXGI/DComp/D2D)
    ├── pencursor-android/      # Android KernelSU daemon backend (epoll/inotify evdev, tiny-skia, SurfaceFlinger)
    └── pencursor/              # Binary entrypoint and target platform dispatcher
```

### Crate Responsibilities

| Crate | Description |
| :--- | :--- |
| **`pencursor-core`** | Pure Rust core library with zero OS dependencies. Provides [`Config`], [`ViewportBounds`], [`DeviceAxisRange`] coordinate normalization, vector cursor primitive geometry ([`CrosshairPrimitives`], [`CirclePrimitives`], [`ContactDotPrimitives`]), the lock-free [`StylusStateTracker`], and backend traits ([`OverlayBackend`], [`PlatformRuntime`]). |
| **`pencursor-windows`** | Windows platform backend. Implements dual-track `RawInput` ingestion (`UsagePage 0x0D` HID Digitizers and `RIM_TYPEMOUSE` absolute tablet coordinates), `WH_MOUSE_LL` physical mouse preemption, and a 1-frame-latency `Direct3D 11` + `DXGI` + `DirectComposition` + `Direct2D` click-through overlay renderer. |
| **`pencursor-android`** | Android KernelSU daemon backend. Renders anti-aliased `64x64` vector cursor sprites once at initialization using `tiny-skia`, uploads them to dual `SurfaceControl` hardware layers via `libgui.so` / `libandroid.so`, and drives zero-copy `Transaction::setPosition` updates from an event-driven `epoll` + `inotify` + `timerfd` loop with `evdev` batch coalescing and sysfs USB runtime power management. |
| **`pencursor`** | Top-level executable crate (`default-members`) that dispatches runtime execution to the active target platform backend. |

---

## Key Technical Design

1. **Tear-Free Atomic Coordinate Synchronization**
   - `StylusStateTracker` packs 2D `f32` coordinates (`(x, y)`) into a single 64-bit atomic word (`AtomicU64`) via `pack_coords` and `unpack_coords`, guaranteeing lock-free, tear-free concurrent updates between high-frequency input hooks and the rendering loop.

2. **Zero-Allocation HID Digitizer Parsing (Windows)**
   - `pencursor-windows::hid` reuses thread-local scratch buffers (`HidScratchBuffers`) for `RIDI_PREPARSEDDATA` and `HIDP_VALUE_CAPS` descriptors, eliminating per-packet heap allocations on 500Hz–1000Hz stylus input streams.

3. **Zero-Copy DirectComposition Overlay (Windows)**
   - Creates a topmost, layered, transparent (`WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`) popup window bound to a `DXGI_SWAP_EFFECT_FLIP_DISCARD` premultiplied-alpha swap chain with `SetMaximumFrameLatency(1)`.

4. **Dual Pre-Rendered `64x64` `SurfaceControl` Sprites & `epoll` Coalescing (Android)**
   - `pencursor-android` rasterizes `Hover` and `Contact` sprites once into two `64x64` `SurfaceControl` layers via `tiny-skia`. Runtime updates perform zero pixel copies, submitting only asynchronous (`oneWay`) `SurfaceComposerClient::Transaction` position and visibility mutations while coalescing queued `evdev` `SYN_REPORT` frames per wakeup.

---

## Build & Verification

### Build Release Binary (Windows)

```bash
cargo build --release
```

### Build Release Binary (Android ARM64 KernelSU Daemon)

```bash
cargo build --release --target aarch64-linux-android
```

### Run Unit Tests & Static Analysis

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
```
