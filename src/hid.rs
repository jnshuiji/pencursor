//! HID 数字化仪（手写笔）原始输入报文解析模块
//!
//! 负责从 RawInput 的 HID 数据包中提取绝对物理坐标、笔尖接触状态以及压感数据。

use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::*;
use windows::Win32::UI::WindowsAndMessaging::*;

// USB HID Usage Pages
const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
const USAGE_PAGE_BUTTON: u16 = 0x09;
const USAGE_PAGE_DIGITIZER: u16 = 0x0D;

// USB HID Usages
const USAGE_X: u16 = 0x30;
const USAGE_Y: u16 = 0x31;
const USAGE_TIP_PRESSURE: u16 = 0x30;
const USAGE_TIP_SWITCH: u16 = 0x42;
const USAGE_SECONDARY_TIP_SWITCH: u16 = 0x43;
const USAGE_ERASER: u16 = 0x45;
const USAGE_BUTTON_1: u16 = 0x01;

/// 数字化仪解析结果
#[derive(Debug, Clone, Copy)]
pub struct DigitizerReport {
    /// 虚拟屏幕绝对 X 坐标（像素）
    pub x: f32,
    /// 虚拟屏幕绝对 Y 坐标（像素）
    pub y: f32,
    /// 笔尖是否处于触屏接触状态
    pub is_contact: bool,
}

/// 解析 HID 数字化仪（Digitizer）输入报文
pub fn parse_digitizer_report(hdevice: HANDLE, report: &[u8]) -> Option<DigitizerReport> {
    if hdevice.is_invalid() || report.is_empty() {
        return None;
    }

    let mut size = 0u32;
    unsafe {
        // 1. 获取预解析数据大小
        let _ = GetRawInputDeviceInfoW(Some(hdevice), RIDI_PREPARSEDDATA, None, &mut size);
        if size == 0 {
            return None;
        }

        // 2. 获取预解析描述符数据
        let mut preparsed = vec![0u8; size as usize];
        let res = GetRawInputDeviceInfoW(
            Some(hdevice),
            RIDI_PREPARSEDDATA,
            Some(preparsed.as_mut_ptr() as _),
            &mut size,
        );
        if res == u32::MAX {
            return None;
        }

        let p_data = PHIDP_PREPARSED_DATA(preparsed.as_mut_ptr() as _);
        let mut raw_x = 0u32;
        let mut raw_y = 0u32;

        // 3. 读取绝对 X / Y 原始坐标
        let st_x = HidP_GetUsageValue(
            HIDP_REPORT_TYPE(0),
            USAGE_PAGE_GENERIC_DESKTOP,
            None,
            USAGE_X,
            &mut raw_x,
            p_data,
            report,
        );
        let st_y = HidP_GetUsageValue(
            HIDP_REPORT_TYPE(0),
            USAGE_PAGE_GENERIC_DESKTOP,
            None,
            USAGE_Y,
            &mut raw_y,
            p_data,
            report,
        );

        // 4. 读取笔尖压感（Tip Pressure: Usage Page 0x0D, Usage 0x30）
        let mut pressure = 0u32;
        let st_press = HidP_GetUsageValue(
            HIDP_REPORT_TYPE(0),
            USAGE_PAGE_DIGITIZER,
            None,
            USAGE_TIP_PRESSURE,
            &mut pressure,
            p_data,
            report,
        );
        let has_pressure = st_press.is_ok() && pressure > 0;

        // 5. 读取按键物理状态（Tip Switch 0x42、Eraser 0x45、Button 1 等）
        let mut buttons = [USAGE_AND_PAGE::default(); 32];
        let mut num_buttons = buttons.len() as u32;
        let st_btn = HidP_GetUsagesEx(
            HIDP_REPORT_TYPE(0),
            None,
            buttons.as_mut_ptr(),
            &mut num_buttons,
            p_data,
            report,
        );
        let mut has_tip_button = false;
        if st_btn.is_ok() {
            for b in &buttons[..num_buttons as usize] {
                if (b.UsagePage == USAGE_PAGE_DIGITIZER
                    && (b.Usage == USAGE_TIP_SWITCH
                        || b.Usage == USAGE_SECONDARY_TIP_SWITCH
                        || b.Usage == USAGE_ERASER))
                    || (b.UsagePage == USAGE_PAGE_BUTTON && b.Usage == USAGE_BUTTON_1)
                {
                    has_tip_button = true;
                    break;
                }
            }
        }

        // 6. 兜底方案：读取数值型 Tip Switch 开关
        let mut tip_val = 0u32;
        let st_tip = HidP_GetUsageValue(
            HIDP_REPORT_TYPE(0),
            USAGE_PAGE_DIGITIZER,
            None,
            USAGE_TIP_SWITCH,
            &mut tip_val,
            p_data,
            report,
        );
        let has_tip_val = st_tip.is_ok() && tip_val > 0;

        // 只要命中任一维度，即判定笔尖触屏成立
        let is_contact = has_pressure || has_tip_button || has_tip_val;

        // 7. 坐标有效性校验与多屏归一化映射
        if st_x.is_ok() && st_y.is_ok() && (raw_x > 0 || raw_y > 0) {
            let mut caps = HIDP_CAPS::default();
            let _ = HidP_GetCaps(p_data, &mut caps);

            let mut value_caps =
                vec![HIDP_VALUE_CAPS::default(); caps.NumberInputValueCaps as usize];
            let mut num_caps = caps.NumberInputValueCaps;
            let _ = HidP_GetValueCaps(
                HIDP_REPORT_TYPE(0),
                value_caps.as_mut_ptr(),
                &mut num_caps,
                p_data,
            );

            let mut max_x = 65535.0f32;
            let mut max_y = 65535.0f32;
            for cap in &value_caps {
                if cap.UsagePage == USAGE_PAGE_GENERIC_DESKTOP
                    && cap.Anonymous.NotRange.Usage == USAGE_X
                {
                    max_x = cap.LogicalMax as f32;
                } else if cap.UsagePage == USAGE_PAGE_GENERIC_DESKTOP
                    && cap.Anonymous.NotRange.Usage == USAGE_Y
                {
                    max_y = cap.LogicalMax as f32;
                }
            }

            let vx = GetSystemMetrics(SM_XVIRTUALSCREEN) as f32;
            let vy = GetSystemMetrics(SM_YVIRTUALSCREEN) as f32;
            let vcx = GetSystemMetrics(SM_CXVIRTUALSCREEN) as f32;
            let vcy = GetSystemMetrics(SM_CYVIRTUALSCREEN) as f32;

            let px = vx + (raw_x as f32 / max_x) * vcx;
            let py = vy + (raw_y as f32 / max_y) * vcy;

            return Some(DigitizerReport {
                x: px,
                y: py,
                is_contact,
            });
        }
    }

    None
}
