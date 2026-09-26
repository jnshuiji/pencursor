//! 全屏透明硬件加速覆层与光标绘制模块
//!
//! 基于 DirectComposition + Direct3D 11 + Direct2D 实现系统级置顶无边框透明窗口。
//! 接收全屏幕 Raw Input 报文并进行零延迟矢量渲染。

use crate::config::{Config, CursorStyle};
use crate::hid::parse_digitizer_report;
use crate::stylus::StylusSnapshot;
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
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Input::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Vector2;

/// Windows Tablet / Stylus 硬件消息特征掩码
const MI_WP_SIGNATURE: usize = 0xFF515700;
const SIGNATURE_MASK: usize = 0xFFFFFF00;
const TOUCH_MASK: usize = 0x80;

/// 覆层窗口底层消息回调过程
unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_INPUT => {
            let mut raw_bytes = [0u8; 1024];
            let mut size = raw_bytes.len() as u32;
            let res = unsafe {
                GetRawInputData(
                    HRAWINPUT(lparam.0 as _),
                    RID_INPUT,
                    Some(raw_bytes.as_mut_ptr() as _),
                    &mut size,
                    std::mem::size_of::<RAWINPUTHEADER>() as u32,
                )
            };

            if res != u32::MAX {
                let raw = unsafe { &*(raw_bytes.as_ptr() as *const RAWINPUT) };

                if raw.header.dwType == RIM_TYPEMOUSE.0 {
                    // 1. 处理鼠标/绝对数位板报文
                    let mouse = unsafe { raw.data.mouse };
                    let is_absolute = (mouse.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0) != 0;
                    let extra = mouse.ulExtraInformation as usize;
                    let is_tablet = (extra & SIGNATURE_MASK) == MI_WP_SIGNATURE;
                    let is_pen = is_tablet && ((extra & TOUCH_MASK) == 0);

                    if is_pen || is_absolute {
                        let (px, py) = if is_absolute {
                            let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) } as f32;
                            let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) } as f32;
                            let vcx = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) } as f32;
                            let vcy = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) } as f32;
                            (
                                vx + (mouse.lLastX as f32 / 65535.0) * vcx,
                                vy + (mouse.lLastY as f32 / 65535.0) * vcy,
                            )
                        } else {
                            let mut pt = POINT::default();
                            unsafe {
                                let _ = GetCursorPos(&mut pt);
                            }
                            (pt.x as f32, pt.y as f32)
                        };

                        let btn_flags = unsafe { mouse.Anonymous.Anonymous.usButtonFlags } as u32;
                        let btn_down = (btn_flags
                            & (RI_MOUSE_LEFT_BUTTON_DOWN | RI_MOUSE_BUTTON_1_DOWN))
                            != 0;
                        let btn_up =
                            (btn_flags & (RI_MOUSE_LEFT_BUTTON_UP | RI_MOUSE_BUTTON_1_UP)) != 0;
                        let key_down =
                            unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 };

                        let contact_update = if btn_down || key_down {
                            Some(true)
                        } else if btn_up {
                            Some(false)
                        } else {
                            None
                        };

                        crate::stylus::mark_pen_active(px, py, contact_update);
                    } else if mouse.lLastX != 0 || mouse.lLastY != 0 {
                        crate::stylus::mark_mouse_active(100);
                    }
                } else if raw.header.dwType == RIM_TYPEHID.0 {
                    // 2. 处理原生 HID 数字化仪报文（全面覆盖 Windows Ink 现代应用与触笔驱动）
                    let dw_size = unsafe { raw.data.hid.dwSizeHid } as usize;
                    let dw_count = unsafe { raw.data.hid.dwCount } as usize;
                    if dw_size > 0 && dw_count > 0 {
                        let report_offset =
                            std::mem::size_of::<RAWINPUTHEADER>() + std::mem::size_of::<u32>() * 2;
                        if report_offset + dw_size <= raw_bytes.len() {
                            let report = &raw_bytes[report_offset..report_offset + dw_size];
                            if let Some(digitizer) =
                                parse_digitizer_report(raw.header.hDevice, report)
                            {
                                crate::stylus::mark_pen_active(
                                    digitizer.x,
                                    digitizer.y,
                                    Some(digitizer.is_contact),
                                );
                            }
                        }
                    }
                }
            }

            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// GPU 硬件加速透明覆层渲染器
#[allow(dead_code)]
pub struct OverlayRenderer {
    pub hwnd: HWND,
    pub vx: i32,
    pub vy: i32,
    pub width: u32,
    pub height: u32,
    swapchain: IDXGISwapChain1,
    _dcomp_target: IDCompositionTarget,
    d2d_context: ID2D1DeviceContext,
    white_brush: ID2D1SolidColorBrush,
    black_brush: ID2D1SolidColorBrush,
    cyan_brush: ID2D1SolidColorBrush,
    was_visible: bool,
}

impl OverlayRenderer {
    /// 初始化覆层窗口及 DirectX / DirectComposition / Direct2D 渲染环境
    pub fn new() -> Result<Self> {
        let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) } as u32;
        let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) } as u32;

        let h_instance = unsafe { GetModuleHandleW(None)? };
        let class_name = w!("PenCursorOverlayWindow");

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(overlay_wnd_proc),
            hInstance: h_instance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        unsafe {
            let _ = RegisterClassExW(&wc);
        }

        // 创建全屏透明顶层穿透窗口
        let hwnd = unsafe {
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

        // 注册系统级 Raw Input 设备：
        // 1. Digitizers (UsagePage 0x0D, 全设备): 确保在所有现代应用内全局接收笔尖硬件报文
        // 2. Mouse (UsagePage 0x01, Usage 0x02): 用于物理鼠标移动互斥及数位板绝对鼠标报文
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
        unsafe {
            let _ = RegisterRawInputDevices(&rids, std::mem::size_of::<RAWINPUTDEVICE>() as u32);
        }

        // 初始化 Direct3D 11 设备
        let mut d3d_device: Option<ID3D11Device> = None;
        let mut d3d_context: Option<ID3D11DeviceContext> = None;
        let feature_levels = [
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
        ];

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

        let d3d_device = d3d_device.unwrap();
        let dxgi_device: IDXGIDevice = d3d_device.cast()?;
        let dxgi_device1: IDXGIDevice1 = d3d_device.cast()?;

        // 设置最大帧排队深度为 1 帧，消除 GPU 缓冲队列引起的排队延迟
        unsafe {
            let _ = dxgi_device1.SetMaximumFrameLatency(1);
        }

        let dxgi_adapter = unsafe { dxgi_device.GetAdapter()? };
        let dxgi_factory: IDXGIFactory2 = unsafe { dxgi_adapter.GetParent()? };

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

        let swapchain = unsafe {
            dxgi_factory.CreateSwapChainForComposition(&dxgi_device, &swapchain_desc, None)?
        };

        // 初始化 DirectComposition 合成目标
        let dcomp_device: IDCompositionDevice =
            unsafe { DCompositionCreateDevice(Some(&dxgi_device))? };

        let dcomp_target = unsafe { dcomp_device.CreateTargetForHwnd(hwnd, true)? };
        let visual = unsafe { dcomp_device.CreateVisual()? };
        unsafe {
            visual.SetContent(&swapchain)?;
            dcomp_target.SetRoot(&visual)?;
            dcomp_device.Commit()?;
        }

        // 初始化 Direct2D 矢量渲染工厂与上下文
        let d2d_factory: ID2D1Factory1 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let d2d_device = unsafe { d2d_factory.CreateDevice(&dxgi_device)? };
        let d2d_context =
            unsafe { d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)? };

        // 创建高对比度画刷
        let white_brush = unsafe {
            d2d_context.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                None,
            )?
        };

        let black_brush = unsafe {
            d2d_context.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.05,
                    g: 0.05,
                    b: 0.05,
                    a: 0.95,
                },
                None,
            )?
        };

        let cyan_brush = unsafe {
            d2d_context.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.0,
                    g: 0.9,
                    b: 1.0,
                    a: 1.0,
                },
                None,
            )?
        };

        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = UpdateWindow(hwnd);
        }

        Ok(Self {
            hwnd,
            vx,
            vy,
            width,
            height,
            swapchain,
            _dcomp_target: dcomp_target,
            d2d_context,
            white_brush,
            black_brush,
            cyan_brush,
            was_visible: false,
        })
    }

    /// 渲染一帧光标状态
    pub fn render_frame(&mut self, snapshot: &StylusSnapshot, config: &Config) -> Result<()> {
        if !snapshot.in_range {
            if self.was_visible {
                unsafe { self.clear_screen()? };
                self.was_visible = false;
            }
            return Ok(());
        }

        self.was_visible = true;

        let surface: IDXGISurface = unsafe { self.swapchain.GetBuffer(0)? };
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

        let target_bitmap = unsafe {
            self.d2d_context
                .CreateBitmapFromDxgiSurface(&surface, Some(&bitmap_props))?
        };

        unsafe {
            self.d2d_context.SetTarget(&target_bitmap);
            self.d2d_context.BeginDraw();
            self.d2d_context.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));

            let px = snapshot.x - self.vx as f32;
            let py = snapshot.y - self.vy as f32;

            if snapshot.is_contact {
                self.draw_contact_dot(px, py, config.contact_dot_radius);
            } else {
                match config.style {
                    CursorStyle::Crosshair => {
                        self.draw_crosshair(px, py, config);
                    }
                    CursorStyle::Circle => {
                        self.draw_circle(px, py, config);
                    }
                    CursorStyle::Both => {
                        self.draw_circle(px, py, config);
                        self.draw_crosshair(px, py, config);
                    }
                }
            }

            let _ = self.d2d_context.EndDraw(None, None);
            self.d2d_context.SetTarget(None);
            drop(target_bitmap);
            drop(surface);

            self.swapchain.Present(0, DXGI_PRESENT(0)).ok()?;
        }

        Ok(())
    }

    /// 清空覆层屏幕画面
    unsafe fn clear_screen(&self) -> Result<()> {
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
            let _ = self.d2d_context.EndDraw(None, None);
            self.d2d_context.SetTarget(None);
            drop(target_bitmap);
            drop(surface);

            self.swapchain.Present(0, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    /// 绘制触屏接触微缩点
    unsafe fn draw_contact_dot(&self, px: f32, py: f32, radius: f32) {
        unsafe {
            self.d2d_context.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 { X: px, Y: py },
                    radiusX: radius + 1.2,
                    radiusY: radius + 1.2,
                },
                &self.black_brush,
            );

            self.d2d_context.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 { X: px, Y: py },
                    radiusX: radius,
                    radiusY: radius,
                },
                &self.cyan_brush,
            );
        }
    }

    /// 绘制高对比度十字准星
    unsafe fn draw_crosshair(&self, px: f32, py: f32, config: &Config) {
        let gap = config.crosshair_gap;
        let size = config.crosshair_size;
        let thick = config.crosshair_thickness;
        let border_thick = thick + 1.6;

        let arms = [
            (
                Vector2 {
                    X: px,
                    Y: py - gap - size,
                },
                Vector2 { X: px, Y: py - gap },
            ),
            (
                Vector2 { X: px, Y: py + gap },
                Vector2 {
                    X: px,
                    Y: py + gap + size,
                },
            ),
            (
                Vector2 {
                    X: px - gap - size,
                    Y: py,
                },
                Vector2 { X: px - gap, Y: py },
            ),
            (
                Vector2 { X: px + gap, Y: py },
                Vector2 {
                    X: px + gap + size,
                    Y: py,
                },
            ),
        ];

        unsafe {
            // 先画黑底阴影描边
            for (p0, p1) in &arms {
                let _ = self
                    .d2d_context
                    .DrawLine(*p0, *p1, &self.black_brush, border_thick, None);
            }

            // 再画白色准星核心线条
            for (p0, p1) in &arms {
                let _ = self
                    .d2d_context
                    .DrawLine(*p0, *p1, &self.white_brush, thick, None);
            }

            // 中心微米定位点
            self.d2d_context.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 { X: px, Y: py },
                    radiusX: 1.0,
                    radiusY: 1.0,
                },
                &self.white_brush,
            );
        }
    }

    /// 绘制高对比度笔刷圆环
    unsafe fn draw_circle(&self, px: f32, py: f32, config: &Config) {
        let r = config.circle_radius;
        let thick = config.circle_thickness;

        let ellipse = D2D1_ELLIPSE {
            point: Vector2 { X: px, Y: py },
            radiusX: r,
            radiusY: r,
        };

        unsafe {
            // 外圈黑色阴影描边
            self.d2d_context
                .DrawEllipse(&ellipse, &self.black_brush, thick + 1.6, None);
            // 内圈白色核心线条
            self.d2d_context
                .DrawEllipse(&ellipse, &self.white_brush, thick, None);

            // 中心微米微点
            self.d2d_context.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 { X: px, Y: py },
                    radiusX: 1.6,
                    radiusY: 1.6,
                },
                &self.black_brush,
            );
            self.d2d_context.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 { X: px, Y: py },
                    radiusX: 1.0,
                    radiusY: 1.0,
                },
                &self.white_brush,
            );
        }
    }
}
