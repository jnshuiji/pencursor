//! USB HID Digitizer raw input report parser.
//!
//! Extracts absolute physical coordinates, tip switch status, and tip pressure from Win32
//! `RawInput` HID packets. Reuses thread-local scratch buffers (`HidScratchBuffers`) to
//! eliminate heap allocations on the 500Hz–1000Hz input hot path.

use crate::input::current_virtual_screen_bounds;
use pencursor_core::DeviceAxisRange;
use std::cell::RefCell;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::*;

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

/// Parsed HID digitizer input state mapped into virtual desktop coordinates.
#[derive(Debug, Clone, Copy)]
pub struct DigitizerReport {
    /// Absolute X coordinate in virtual screen space (pixels).
    pub x: f32,
    /// Absolute Y coordinate in virtual screen space (pixels).
    pub y: f32,
    /// Whether the stylus tip is in physical contact with the digitizer surface.
    pub is_contact: bool,
}

/// Thread-local reusable buffers for HID preparsed data and value capability descriptors.
#[derive(Default)]
struct HidScratchBuffers {
    preparsed: Vec<u8>,
    value_caps: Vec<HIDP_VALUE_CAPS>,
}

thread_local! {
    static HID_SCRATCH: RefCell<HidScratchBuffers> = RefCell::new(HidScratchBuffers::default());
}

/// Parses a single Win32 `RawInput` HID digitizer report packet.
pub fn parse_digitizer_report(hdevice: HANDLE, report: &[u8]) -> Option<DigitizerReport> {
    if hdevice.is_invalid() || report.is_empty() {
        return None;
    }

    HID_SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        let mut size = 0u32;

        // SAFETY: Passing `None` as the output buffer queries the required byte size of `RIDI_PREPARSEDDATA`.
        unsafe {
            let _ = GetRawInputDeviceInfoW(Some(hdevice), RIDI_PREPARSEDDATA, None, &mut size);
        }
        if size == 0 {
            return None;
        }

        scratch.preparsed.resize(size as usize, 0);

        // SAFETY: `scratch.preparsed` is resized to at least `size` contiguous writable bytes.
        let res = unsafe {
            GetRawInputDeviceInfoW(
                Some(hdevice),
                RIDI_PREPARSEDDATA,
                Some(scratch.preparsed.as_mut_ptr().cast()),
                &mut size,
            )
        };
        if res == u32::MAX {
            return None;
        }

        let p_data = PHIDP_PREPARSED_DATA(scratch.preparsed.as_mut_ptr() as isize);
        let mut raw_x = 0u32;
        let mut raw_y = 0u32;

        // SAFETY: `p_data` points to valid HID preparsed data and `report` references the raw HID report slice.
        let (st_x, st_y, is_contact) = unsafe {
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

            // Query Tip Pressure (Usage Page 0x0D, Usage 0x30).
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

            // Query active button usages (Tip Switch 0x42, Secondary Tip Switch 0x43, Eraser 0x45, Button 1).
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

            // Fallback: query numeric Tip Switch usage value.
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

            (st_x, st_y, has_pressure || has_tip_button || has_tip_val)
        };

        // Validate coordinates and map logical device bounds into the virtual desktop viewport.
        if st_x.is_ok() && st_y.is_ok() && (raw_x > 0 || raw_y > 0) {
            let mut caps = HIDP_CAPS::default();
            // SAFETY: Queries top-level HID collection capabilities from valid preparsed data.
            unsafe {
                let _ = HidP_GetCaps(p_data, &mut caps);
            }

            let num_caps_usize = caps.NumberInputValueCaps as usize;
            scratch
                .value_caps
                .resize(num_caps_usize, HIDP_VALUE_CAPS::default());
            let mut num_caps = caps.NumberInputValueCaps;

            if num_caps > 0 {
                // SAFETY: `scratch.value_caps` has capacity for at least `num_caps` elements.
                unsafe {
                    let _ = HidP_GetValueCaps(
                        HIDP_REPORT_TYPE(0),
                        scratch.value_caps.as_mut_ptr(),
                        &mut num_caps,
                        p_data,
                    );
                }
            }

            let mut max_x = 65535.0_f32;
            let mut max_y = 65535.0_f32;
            for cap in &scratch.value_caps[..num_caps as usize] {
                // SAFETY: Reads the non-range `Usage` identifier from the `Anonymous` union.
                let usage = unsafe { cap.Anonymous.NotRange.Usage };
                if cap.UsagePage == USAGE_PAGE_GENERIC_DESKTOP && usage == USAGE_X {
                    max_x = cap.LogicalMax as f32;
                } else if cap.UsagePage == USAGE_PAGE_GENERIC_DESKTOP && usage == USAGE_Y {
                    max_y = cap.LogicalMax as f32;
                }
            }

            let viewport = current_virtual_screen_bounds();
            let (mapped_x, mapped_y) = DeviceAxisRange::from_max(max_x, max_y).map_to_viewport(
                raw_x as f32,
                raw_y as f32,
                viewport,
            );

            return Some(DigitizerReport {
                x: mapped_x,
                y: mapped_y,
                is_contact,
            });
        }

        None
    })
}
