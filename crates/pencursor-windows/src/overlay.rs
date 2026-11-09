//! Fullscreen transparent hardware-accelerated overlay and vector cursor renderer.
//!
//! Combines a click-through topmost Win32 layered popup window with Direct3D 11, DXGI
//! (configured for 1-frame maximum latency), DirectComposition, and Direct2D for zero-copy
//! vector cursor composition.

use crate::input::{current_virtual_screen_bounds, process_wm_input};
use pencursor_core::{
    CirclePrimitives, Config, ContactDotPrimitives, CrosshairPrimitives, CursorPalette,
    CursorStyle, OverlayBackend, Point2f, RgbaColor, StylusSnapshot, ViewportBounds,
};
use std::mem::ManuallyDrop;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Vector2;

#[inline]
const fn to_d2d_color(color: RgbaColor) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: color.r,
        g: color.g,
        b: color.b,
        a: color.a,
    }
}

#[inline]
const fn to_vec2(pt: Point2f) -> Vector2 {
    Vector2 { X: pt.x, Y: pt.y }
}

/// Window procedure for the transparent overlay `HWND`.
unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_INPUT => {
            process_wm_input(lparam);
            // SAFETY: Passes `WM_INPUT` to `DefWindowProcW` for default system cleanup.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_DESTROY => {
            // SAFETY: Posts `WM_QUIT` to terminate the message loop when the overlay window is destroyed.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: Delegates unhandled window messages to the default Win32 window procedure.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// GPU-accelerated DirectComposition + Direct2D overlay renderer.
#[derive(Debug)]
pub struct OverlayRenderer {
    viewport: ViewportBounds,
    swapchain: IDXGISwapChain1,
    _dcomp_target: IDCompositionTarget,
    d2d_context: ID2D1DeviceContext,
    primary_brush: ID2D1SolidColorBrush,
    outline_brush: ID2D1SolidColorBrush,
    contact_brush: ID2D1SolidColorBrush,
    was_visible: bool,
}

impl OverlayRenderer {
    /// Initializes the overlay window and DirectX rendering pipeline using [`CursorPalette::default`].
    pub fn new() -> Result<Self> {
        Self::with_palette(CursorPalette::default())
    }

    /// Initializes the overlay window and DirectX rendering pipeline with the specified [`CursorPalette`].
    pub fn with_palette(palette: CursorPalette) -> Result<Self> {
        let viewport = current_virtual_screen_bounds();
        let vx = viewport.origin_x as i32;
        let vy = viewport.origin_y as i32;
        let width = viewport.width.max(1.0) as u32;
        let height = viewport.height.max(1.0) as u32;

        // SAFETY: Registers the window class and creates a topmost, click-through popup `HWND`.
        let hwnd = unsafe {
            let h_instance = GetModuleHandleW(None)?;
            let class_name = w!("PenCursorOverlayWindow");

            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(overlay_wnd_proc),
                hInstance: h_instance.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            let _ = RegisterClassExW(&wc);

            CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW,
                class_name,
                w!("PenCursorOverlay"),
                WS_POPUP,
                vx,
                vy,
                width as i32,
                height as i32,
                None,
                None,
                Some(h_instance.into()),
                None,
            )?
        };

        // Register global RawInput sinks on the overlay window:
        // 1. Digitizers (UsagePage 0x0D, RIDEV_PAGEONLY) for system-wide stylus reports.
        // 2. Mouse (UsagePage 0x01, Usage 0x02) for absolute tablet mouse mode and physical mouse preemption.
        let rids = [
            RAWINPUTDEVICE {
                usUsagePage: 0x0D,
                usUsage: 0x00,
                dwFlags: RIDEV_INPUTSINK | RIDEV_PAGEONLY,
                hwndTarget: hwnd,
            },
            RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x02,
                dwFlags: RIDEV_INPUTSINK,
                hwndTarget: hwnd,
            },
        ];
        // SAFETY: `rids` is a valid two-element `RAWINPUTDEVICE` slice targeting `hwnd`.
        unsafe {
            let _ = RegisterRawInputDevices(&rids, size_of::<RAWINPUTDEVICE>() as u32);
        }

        // Create hardware Direct3D 11 device with BGRA support for Direct2D interop.
        let mut d3d_device: Option<ID3D11Device> = None;
        let mut d3d_context: Option<ID3D11DeviceContext> = None;
        let feature_levels = [
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
        ];

        // SAFETY: Standard `D3D11CreateDevice` initialization with hardware driver and BGRA support.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut d3d_device),
                None,
                Some(&mut d3d_context),
            )?;
        }

        let d3d_device =
            d3d_device.ok_or_else(|| Error::new(E_FAIL, "D3D11CreateDevice returned None"))?;
        let dxgi_device: IDXGIDevice = d3d_device.cast()?;
        let dxgi_device1: IDXGIDevice1 = d3d_device.cast()?;

        // SAFETY: Configures 1-frame DXGI latency, composition swap chain, DirectComposition visual tree, and Direct2D brushes.
        let (swapchain, dcomp_target, d2d_context, primary_brush, outline_brush, contact_brush) = unsafe {
            let _ = dxgi_device1.SetMaximumFrameLatency(1);

            let dxgi_adapter = dxgi_device.GetAdapter()?;
            let dxgi_factory: IDXGIFactory2 = dxgi_adapter.GetParent()?;

            let swapchain_desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: BOOL(0),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: 0,
            };

            let swapchain =
                dxgi_factory.CreateSwapChainForComposition(&dxgi_device, &swapchain_desc, None)?;

            let dcomp_device: IDCompositionDevice = DCompositionCreateDevice(Some(&dxgi_device))?;
            let dcomp_target = dcomp_device.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp_device.CreateVisual()?;
            visual.SetContent(&swapchain)?;
            dcomp_target.SetRoot(&visual)?;
            dcomp_device.Commit()?;

            let d2d_factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d_device = d2d_factory.CreateDevice(&dxgi_device)?;
            let d2d_context = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;

            let primary_brush = d2d_context
                .CreateSolidColorBrush(&to_d2d_color(palette.primary), None)?;
            let outline_brush = d2d_context
                .CreateSolidColorBrush(&to_d2d_color(palette.outline), None)?;
            let contact_brush = d2d_context
                .CreateSolidColorBrush(&to_d2d_color(palette.contact), None)?;

            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = UpdateWindow(hwnd);

            (
                swapchain,
                dcomp_target,
                d2d_context,
                primary_brush,
                outline_brush,
                contact_brush,
            )
        };

        Ok(Self {
            viewport,
            swapchain,
            _dcomp_target: dcomp_target,
            d2d_context,
            primary_brush,
            outline_brush,
            contact_brush,
            was_visible: false,
        })
    }

    /// Renders a single cursor overlay frame according to `snapshot` and `config`.
    pub fn render_frame(&mut self, snapshot: &StylusSnapshot, config: &Config) -> Result<()> {
        if !snapshot.in_range {
            if self.was_visible {
                self.present_d2d_frame(|_| {})?;
                self.was_visible = false;
            }
            return Ok(());
        }

        self.was_visible = true;
        let center = self.viewport.to_local(snapshot.x, snapshot.y);

        self.present_d2d_frame(|renderer| {
            if snapshot.is_contact {
                let dot = ContactDotPrimitives::compute(center, config);
                renderer.draw_contact_dot(&dot);
            } else {
                match config.style {
                    CursorStyle::Crosshair => {
                        let cross = CrosshairPrimitives::compute(center, config);
                        renderer.draw_crosshair(&cross);
                    }
                    CursorStyle::Circle => {
                        let circle = CirclePrimitives::compute(center, config);
                        renderer.draw_circle(&circle);
                    }
                    CursorStyle::Both => {
                        let circle = CirclePrimitives::compute(center, config);
                        let cross = CrosshairPrimitives::compute(center, config);
                        renderer.draw_circle(&circle);
                        renderer.draw_crosshair(&cross);
                    }
                }
            }
        })
    }

    /// Acquires the swap chain backbuffer, clears it to transparent, executes `draw_commands`, and presents.
    fn present_d2d_frame(&self, draw_commands: impl FnOnce(&Self)) -> Result<()> {
        // SAFETY: Binds swap chain buffer 0 as a Direct2D target bitmap, executes draw calls, unbinds before releasing, and presents with sync interval 0.
        unsafe {
            let surface: IDXGISurface = self.swapchain.GetBuffer(0)?;
            let bitmap_props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: ManuallyDrop::new(None),
            };

            let target_bitmap = self
                .d2d_context
                .CreateBitmapFromDxgiSurface(&surface, Some(&bitmap_props))?;

            self.d2d_context.SetTarget(&target_bitmap);
            self.d2d_context.BeginDraw();
            self.d2d_context.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));

            draw_commands(self);

            let _ = self.d2d_context.EndDraw(None, None);
            self.d2d_context.SetTarget(None);
            drop(target_bitmap);
            drop(surface);

            self.swapchain.Present(0, DXGI_PRESENT(0)).ok()?;
        }

        Ok(())
    }

    /// Draws the contracted contact indicator dot.
    fn draw_contact_dot(&self, dot: &ContactDotPrimitives) {
        let pt = to_vec2(dot.center);
        // SAFETY: Invoked within an active `BeginDraw` / `EndDraw` scope on `self.d2d_context`.
        unsafe {
            self.d2d_context.FillEllipse(
                &D2D1_ELLIPSE {
                    point: pt,
                    radiusX: dot.outline_radius,
                    radiusY: dot.outline_radius,
                },
                &self.outline_brush,
            );

            self.d2d_context.FillEllipse(
                &D2D1_ELLIPSE {
                    point: pt,
                    radiusX: dot.inner_radius,
                    radiusY: dot.inner_radius,
                },
                &self.contact_brush,
            );
        }
    }

    /// Draws the high-contrast crosshair cursor.
    fn draw_crosshair(&self, cross: &CrosshairPrimitives) {
        // SAFETY: Invoked within an active `BeginDraw` / `EndDraw` scope on `self.d2d_context`.
        unsafe {
            for seg in &cross.arms {
                self.d2d_context.DrawLine(
                    to_vec2(seg.start),
                    to_vec2(seg.end),
                    &self.outline_brush,
                    cross.outline_thickness,
                    None,
                );
            }

            for seg in &cross.arms {
                self.d2d_context.DrawLine(
                    to_vec2(seg.start),
                    to_vec2(seg.end),
                    &self.primary_brush,
                    cross.inner_thickness,
                    None,
                );
            }

            self.d2d_context.FillEllipse(
                &D2D1_ELLIPSE {
                    point: to_vec2(cross.center),
                    radiusX: cross.center_dot_radius,
                    radiusY: cross.center_dot_radius,
                },
                &self.primary_brush,
            );
        }
    }

    /// Draws the high-contrast circular brush ring and center dot.
    fn draw_circle(&self, circle: &CirclePrimitives) {
        let pt = to_vec2(circle.center);
        let ellipse = D2D1_ELLIPSE {
            point: pt,
            radiusX: circle.radius,
            radiusY: circle.radius,
        };

        // SAFETY: Invoked within an active `BeginDraw` / `EndDraw` scope on `self.d2d_context`.
        unsafe {
            self.d2d_context.DrawEllipse(
                &ellipse,
                &self.outline_brush,
                circle.outline_thickness,
                None,
            );
            self.d2d_context.DrawEllipse(
                &ellipse,
                &self.primary_brush,
                circle.inner_thickness,
                None,
            );

            self.d2d_context.FillEllipse(
                &D2D1_ELLIPSE {
                    point: pt,
                    radiusX: circle.center_outer_dot_radius,
                    radiusY: circle.center_outer_dot_radius,
                },
                &self.outline_brush,
            );
            self.d2d_context.FillEllipse(
                &D2D1_ELLIPSE {
                    point: pt,
                    radiusX: circle.center_inner_dot_radius,
                    radiusY: circle.center_inner_dot_radius,
                },
                &self.primary_brush,
            );
        }
    }
}

impl OverlayBackend for OverlayRenderer {
    type Error = Error;

    #[inline]
    fn render_frame(&mut self, snapshot: &StylusSnapshot, config: &Config) -> Result<()> {
        Self::render_frame(self, snapshot, config)
    }
}
