# PenCursor

A low-latency, hardware-accelerated stylus cursor overlay service engineered for external drawing tablets and pen digitizers. Built on a modular cross-platform Rust 2024 workspace powered by the Linebender 2D graphics ecosystem (`kurbo`, `peniko`, and `vello_cpu`), supporting Windows (`Win32 RawInput` + `DirectComposition`) and Android (`KernelSU Daemon` + `evdev` + `SurfaceFlinger`).

---

## Architecture Overview

PenCursor follows a Ports & Adapters (Hexagonal) workspace structure that centralizes all domain models, state synchronization, and Linebender `vello_cpu` vector cursor rasterization inside `pencursor-core`, while delegating hardware sprite composition to lightweight OS backends:

```text
pencursor/
├── Cargo.toml                  # Workspace manifest (Edition 2024, Resolver v3, LTO Release profile)
└── crates/
    ├── pencursor-core/         # Linebender kurbo/peniko geometry, vello_cpu rasterizer, and lock-free state engine
    ├── pencursor-windows/      # Windows backend (RawInput HID/Mouse, WH_MOUSE_LL, D3D11/DXGI/DirectComposition)
    ├── pencursor-android/      # Android KernelSU daemon backend (epoll/inotify evdev, SurfaceFlinger dual-sprite)
    └── pencursor/              # Binary entrypoint and target platform dispatcher
```

### Crate Responsibilities

| Crate | Description |
| :--- | :--- |
| **`pencursor-core`** | Pure Rust core library built on Linebender `kurbo`, `peniko`, and `vello_cpu`. Provides [`Config`], [`ViewportBounds`], [`DeviceAxisRange`] coordinate normalization, `kurbo` cursor primitive shapes ([`CrosshairPrimitives`], [`CirclePrimitives`], [`ContactDotPrimitives`]), unified `vello_cpu` `64x64` sprite rasterization ([`render_hover_sprite`], [`render_contact_sprite`]), the lock-free [`StylusStateTracker`], and backend traits ([`OverlayBackend`], [`PlatformRuntime`]). |
| **`pencursor-windows`** | Windows platform backend. Implements dual-track `RawInput` ingestion (`UsagePage 0x0D` HID Digitizers and `RIM_TYPEMOUSE` absolute tablet coordinates), `WH_MOUSE_LL` physical mouse preemption, and a zero-runtime-redraw `Direct3D 11` + `DXGI` + `DirectComposition` `64x64` hardware sprite overlay (`IDCompositionVisual::SetOffsetX2` / `SetOffsetY2`). |
| **`pencursor-android`** | Android KernelSU daemon backend. Uploads the `vello_cpu`-rasterized `64x64` sprites once at startup into dual `SurfaceControl` hardware layers via `libgui.so` / `libandroid.so`, and drives zero-copy `Transaction::setPosition` updates from an event-driven `epoll` + `inotify` + `timerfd` loop with `evdev` batch coalescing and sysfs USB runtime power management. |
| **`pencursor`** | Top-level executable crate (`default-members`) that dispatches runtime execution to the active target platform backend. |

---

## Key Technical Design

1. **Unified Linebender `kurbo` + `peniko` + `vello_cpu` Rendering Engine**
   - All cursor vector geometry and color styling are modeled using `kurbo` (`Point`, `Line`, `Circle`, `BezPath`, `Stroke`) and `peniko` (`Color`) in `pencursor-core`, and rasterized via `vello_cpu`'s SIMD sparse-strip pipeline into `64x64` premultiplied RGBA8 sprites shared identically across Windows and Android.

2. **Zero-Runtime-Redraw Hardware Sprite Composition (Windows & Android)**
   - Both platforms upload the `64x64` `Hover` and `Contact` sprites once at startup (`IDXGISwapChain1` + `IDCompositionVisual` on Windows; `SurfaceControl` + `ANativeWindow_lock` on Android). Runtime motion and state transitions execute zero pixel writes, updating only hardware compositor layer offsets (`SetOffsetX2`/`SetOffsetY2` on Windows; `Transaction::setPosition` on Android).

3. **Tear-Free Atomic Coordinate Synchronization**
   - `StylusStateTracker` packs 2D `f32` coordinates (`(x, y)`) into a single 64-bit atomic word (`AtomicU64`) via `pack_coords` and `unpack_coords`, guaranteeing lock-free, tear-free concurrent updates between high-frequency input hooks and the rendering loop.

4. **Zero-Allocation Input Pipelines**
   - `pencursor-windows::hid` reuses thread-local scratch buffers (`HidScratchBuffers`) for `RIDI_PREPARSEDDATA` and `HIDP_VALUE_CAPS` descriptors.
   - `pencursor-android::evdev` drains and coalesces queued `SYN_REPORT` packets per `epoll` wakeup, sleeping with `0.0%` CPU/GPU utilization when the stylus is out of range.

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
