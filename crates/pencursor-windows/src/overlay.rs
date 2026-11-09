//! Hardware-accelerated DirectComposition sprite overlay renderer for Windows.
//!
//! Architecture:
//! - Rasterizes `64x64` premultiplied RGBA8 `Hover` and `Contact` sprites once at startup
//!   using Linebender `vello_cpu` in `pencursor-core`.
//! - Uploads each sprite once into a dedicated `64x64` `DXGI_FORMAT_R8G8B8A8_UNORM` composition
//!   swap chain (`hover_swapchain` and `contact_swapchain`).
//! - Performs zero runtime pixel redraws: cursor motion and hover/contact transitions are
//!   driven purely by `IDCompositionVisual::SetOffsetX2`, `SetOffsetY2`, and `SetContent`.

use crate::input::{current_virtual_screen_bounds, process_wm_input};
use pencursor_core::{
    Config, OverlayBackend, Pixmap, SPRITE_CENTER_OFFSET, SPRITE_SIZE, StylusSnapshot,
    ViewportBounds, render_contact_sprite, render_hover_sprite,
};
use windows::Win32::Foundation::*;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveSprite {
    None,
    Hover,
    Contact,
}

/// GPU-accelerated DirectComposition `64x64` dual-sprite overlay renderer.
#[derive(Debug)]
pub struct OverlayRenderer {
    viewport: ViewportBounds,
    dcomp_device: IDCompositionDevice,
    _dcomp_target: IDCompositionTarget,
    cursor_visual: IDCompositionVisual,
    hover_swapchain: IDXGISwapChain1,
    contact_swapchain: IDXGISwapChain1,
    active_sprite: ActiveSprite,
    last_x: f32,
    last_y: f32,
}

impl OverlayRenderer {
    /// Initializes the overlay window and DirectComposition pipeline using [`Config::default`].
    pub fn new() -> Result<Self> {
        Self::with_config(&Config::default())
    }

    /// Initializes the overlay window, pre-renders `64x64` sprites via Linebender `vello_cpu`,
    /// and uploads them into two `DirectComposition` swap chains.
    pub fn with_config(config: &Config) -> Result<Self> {
        let viewport = current_virtual_screen_bounds();
        let vx = viewport.origin_x as i32;
        let vy = viewport.origin_y as i32;
        let width = viewport.width.max(1.0) as u32;
        let height = viewport.height.max(1.0) as u32;

        let hover_pixmap = render_hover_sprite(config);
        let contact_pixmap = render_contact_sprite(config);

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

        let mut d3d_device: Option<ID3D11Device> = None;
        let mut d3d_context: Option<ID3D11DeviceContext> = None;
        let feature_levels = [
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
        ];

        // SAFETY: Standard `D3D11CreateDevice` initialization with hardware driver.
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
        let d3d_context =
            d3d_context.ok_or_else(|| Error::new(E_FAIL, "D3D11ImmediateContext is None"))?;
        let dxgi_device: IDXGIDevice = d3d_device.cast()?;
        let dxgi_device1: IDXGIDevice1 = d3d_device.cast()?;

        // SAFETY: Configures 1-frame DXGI latency, creates two 64x64 RGBA8 swap chains, uploads `vello_cpu` sprites, and initializes DirectComposition visual tree.
        let (dcomp_device, dcomp_target, cursor_visual, hover_swapchain, contact_swapchain) = unsafe {
            let _ = dxgi_device1.SetMaximumFrameLatency(1);

            let dxgi_adapter = dxgi_device.GetAdapter()?;
            let dxgi_factory: IDXGIFactory2 = dxgi_adapter.GetParent()?;

            let hover_swapchain = Self::create_and_upload_sprite_swapchain(
                &dxgi_factory,
                &dxgi_device,
                &d3d_context,
                &hover_pixmap,
            )?;
            let contact_swapchain = Self::create_and_upload_sprite_swapchain(
                &dxgi_factory,
                &dxgi_device,
                &d3d_context,
                &contact_pixmap,
            )?;

            let dcomp_device: IDCompositionDevice = DCompositionCreateDevice(Some(&dxgi_device))?;
            let dcomp_target = dcomp_device.CreateTargetForHwnd(hwnd, true)?;
            let cursor_visual = dcomp_device.CreateVisual()?;
            cursor_visual.SetContent(None)?;
            dcomp_target.SetRoot(&cursor_visual)?;
            dcomp_device.Commit()?;

            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = UpdateWindow(hwnd);

            (
                dcomp_device,
                dcomp_target,
                cursor_visual,
                hover_swapchain,
                contact_swapchain,
            )
        };

        Ok(Self {
            viewport,
            dcomp_device,
            _dcomp_target: dcomp_target,
            cursor_visual,
            hover_swapchain,
            contact_swapchain,
            active_sprite: ActiveSprite::None,
            last_x: f32::NAN,
            last_y: f32::NAN,
        })
    }

    unsafe fn create_and_upload_sprite_swapchain(
        dxgi_factory: &IDXGIFactory2,
        dxgi_device: &IDXGIDevice,
        d3d_context: &ID3D11DeviceContext,
        pixmap: &Pixmap,
    ) -> Result<IDXGISwapChain1> {
        let sprite_size = u32::from(SPRITE_SIZE);
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: sprite_size,
            Height: sprite_size,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
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

        // SAFETY: Creates a 64x64 composition swap chain, copies the `vello_cpu` RGBA8 buffer into texture 0, and presents it once.
        unsafe {
            let swapchain = dxgi_factory.CreateSwapChainForComposition(dxgi_device, &desc, None)?;
            let backbuffer: ID3D11Texture2D = swapchain.GetBuffer(0)?;
            let row_pitch = sprite_size * 4;
            d3d_context.UpdateSubresource(
                &backbuffer,
                0,
                None,
                pixmap.data_as_u8_slice().as_ptr().cast(),
                row_pitch,
                0,
            );
            swapchain.Present(0, DXGI_PRESENT(0)).ok()?;
            Ok(swapchain)
        }
    }

    /// Updates the DirectComposition visual position and active sprite content for the current frame.
    pub fn render_frame(&mut self, snapshot: &StylusSnapshot, _config: &Config) -> Result<()> {
        let desired = if !snapshot.in_range {
            ActiveSprite::None
        } else if snapshot.is_contact {
            ActiveSprite::Contact
        } else {
            ActiveSprite::Hover
        };

        if desired == ActiveSprite::None && self.active_sprite == ActiveSprite::None {
            return Ok(());
        }

        let local = self.viewport.to_local(snapshot.x, snapshot.y);
        let top_left_x = (local.x as f32) - SPRITE_CENTER_OFFSET;
        let top_left_y = (local.y as f32) - SPRITE_CENTER_OFFSET;

        if desired == self.active_sprite
            && (top_left_x - self.last_x).abs() < 0.05
            && (top_left_y - self.last_y).abs() < 0.05
        {
            return Ok(());
        }

        // SAFETY: Updates the DirectComposition visual offset and swapchain content, then commits the batch to DWM.
        unsafe {
            match desired {
                ActiveSprite::None => {
                    self.cursor_visual.SetContent(None)?;
                }
                ActiveSprite::Hover => {
                    if self.active_sprite != ActiveSprite::Hover {
                        self.cursor_visual.SetContent(&self.hover_swapchain)?;
                    }
                    self.cursor_visual.SetOffsetX2(top_left_x)?;
                    self.cursor_visual.SetOffsetY2(top_left_y)?;
                }
                ActiveSprite::Contact => {
                    if self.active_sprite != ActiveSprite::Contact {
                        self.cursor_visual.SetContent(&self.contact_swapchain)?;
                    }
                    self.cursor_visual.SetOffsetX2(top_left_x)?;
                    self.cursor_visual.SetOffsetY2(top_left_y)?;
                }
            }
            self.dcomp_device.Commit()?;
        }

        self.active_sprite = desired;
        self.last_x = top_left_x;
        self.last_y = top_left_y;
        Ok(())
    }
}

impl OverlayBackend for OverlayRenderer {
    type Error = Error;

    #[inline]
    fn render_frame(&mut self, snapshot: &StylusSnapshot, config: &Config) -> Result<()> {
        Self::render_frame(self, snapshot, config)
    }
}
