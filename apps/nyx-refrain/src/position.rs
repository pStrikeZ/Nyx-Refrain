//! Tray flyout positioning math and monitor work area detection.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskbarEdge {
    Bottom,
    Top,
    Left,
    Right,
}

/// Margin in physical pixels between the panel and work area edges / taskbar.
pub const FLYOUT_MARGIN: i32 = 12;

/// Determines which edge of the monitor the taskbar is on relative to the work area.
pub fn detect_taskbar_edge(icon: Rect, work_area: Rect) -> TaskbarEdge {
    let wa_left = work_area.x;
    let wa_right = work_area.x + work_area.width as i32;
    let wa_top = work_area.y;
    let wa_bottom = work_area.y + work_area.height as i32;

    let icon_cx = icon.x + (icon.width as i32) / 2;
    let icon_cy = icon.y + (icon.height as i32) / 2;

    // 1. Check if the icon lies outside (or predominantly beyond) one of the work area edges.
    if icon.y >= wa_bottom - (icon.height as i32) / 2 {
        return TaskbarEdge::Bottom;
    }
    if icon.y + (icon.height as i32) <= wa_top + (icon.height as i32) / 2 {
        return TaskbarEdge::Top;
    }
    if icon.x >= wa_right - (icon.width as i32) / 2 {
        return TaskbarEdge::Right;
    }
    if icon.x + (icon.width as i32) <= wa_left + (icon.width as i32) / 2 {
        return TaskbarEdge::Left;
    }

    // 2. If the icon is inside the work area (e.g. autohide taskbar), pick the nearest edge.
    let dist_bottom = wa_bottom - icon_cy;
    let dist_top = icon_cy - wa_top;
    let dist_right = wa_right - icon_cx;
    let dist_left = icon_cx - wa_left;

    let mut min_dist = dist_bottom;
    let mut edge = TaskbarEdge::Bottom;

    if dist_top < min_dist {
        min_dist = dist_top;
        edge = TaskbarEdge::Top;
    }
    if dist_right < min_dist {
        min_dist = dist_right;
        edge = TaskbarEdge::Right;
    }
    if dist_left < min_dist {
        edge = TaskbarEdge::Left;
    }

    edge
}

/// Calculates the physical position of the flyout panel based on the tray icon rect,
/// the monitor's work area, and the physical panel size.
///
/// Places the panel:
/// - Above the icon if the taskbar is at the bottom.
/// - Below the icon if the taskbar is at the top.
/// - Beside (left of) the icon if the taskbar is at the right.
/// - Beside (right of) the icon if the taskbar is at the left.
///
/// Clamped inside the work area with a 12 px margin.
pub fn flyout_position(icon: Rect, work_area: Rect, panel_size: Size) -> Pos {
    let edge = detect_taskbar_edge(icon, work_area);

    let icon_cx = icon.x + (icon.width as i32) / 2;
    let icon_cy = icon.y + (icon.height as i32) / 2;

    let pw = panel_size.width as i32;
    let ph = panel_size.height as i32;

    let (target_x, target_y) = match edge {
        TaskbarEdge::Bottom => {
            let x = icon_cx - pw / 2;
            let y = icon.y - ph - FLYOUT_MARGIN;
            (x, y)
        }
        TaskbarEdge::Top => {
            let x = icon_cx - pw / 2;
            let y = icon.y + (icon.height as i32) + FLYOUT_MARGIN;
            (x, y)
        }
        TaskbarEdge::Right => {
            let x = icon.x - pw - FLYOUT_MARGIN;
            let y = icon_cy - ph / 2;
            (x, y)
        }
        TaskbarEdge::Left => {
            let x = icon.x + (icon.width as i32) + FLYOUT_MARGIN;
            let y = icon_cy - ph / 2;
            (x, y)
        }
    };

    clamp_to_work_area(target_x, target_y, work_area, panel_size)
}

/// Places the flyout near the bottom-right of the work area (e.g. for first start or secondary launch).
pub fn fallback_position(work_area: Rect, panel_size: Size) -> Pos {
    let pw = panel_size.width as i32;
    let ph = panel_size.height as i32;

    let target_x = work_area.x + (work_area.width as i32) - pw - FLYOUT_MARGIN;
    let target_y = work_area.y + (work_area.height as i32) - ph - FLYOUT_MARGIN;

    clamp_to_work_area(target_x, target_y, work_area, panel_size)
}

fn clamp_to_work_area(x: i32, y: i32, work_area: Rect, panel_size: Size) -> Pos {
    let wa_left = work_area.x;
    let wa_right = work_area.x + work_area.width as i32;
    let wa_top = work_area.y;
    let wa_bottom = work_area.y + work_area.height as i32;

    let pw = panel_size.width as i32;
    let ph = panel_size.height as i32;

    let min_x = wa_left + FLYOUT_MARGIN;
    let max_x = wa_right - pw - FLYOUT_MARGIN;
    let clamped_x = if max_x >= min_x {
        x.clamp(min_x, max_x)
    } else {
        min_x
    };

    let min_y = wa_top + FLYOUT_MARGIN;
    let max_y = wa_bottom - ph - FLYOUT_MARGIN;
    let clamped_y = if max_y >= min_y {
        y.clamp(min_y, max_y)
    } else {
        min_y
    };

    Pos {
        x: clamped_x,
        y: clamped_y,
    }
}

#[cfg(windows)]
pub fn monitor_work_area_and_dpi_at(pt: (i32, i32)) -> (Rect, f32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

    unsafe {
        let p = POINT { x: pt.0, y: pt.1 };
        let hmon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(hmon, &mut info);
        let rc = info.rcWork;
        let work_area = Rect {
            x: rc.left,
            y: rc.top,
            width: (rc.right - rc.left).max(0) as u32,
            height: (rc.bottom - rc.top).max(0) as u32,
        };

        let mut dpix = 96;
        let mut dpiy = 96;
        let dpi_scale = if GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpix, &mut dpiy).is_ok() {
            (dpix as f32 / 96.0).max(1.0)
        } else {
            1.0
        };

        (work_area, dpi_scale)
    }
}

#[cfg(not(windows))]
pub fn monitor_work_area_and_dpi_at(_pt: (i32, i32)) -> (Rect, f32) {
    (
        Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        },
        1.0,
    )
}

#[cfg(windows)]
pub fn primary_work_area_and_dpi() -> (Rect, f32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

    unsafe {
        let p = POINT { x: 0, y: 0 };
        let hmon = MonitorFromPoint(p, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(hmon, &mut info);
        let rc = info.rcWork;
        let work_area = Rect {
            x: rc.left,
            y: rc.top,
            width: (rc.right - rc.left).max(0) as u32,
            height: (rc.bottom - rc.top).max(0) as u32,
        };

        let mut dpix = 96;
        let mut dpiy = 96;
        let dpi_scale = if GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpix, &mut dpiy).is_ok() {
            (dpix as f32 / 96.0).max(1.0)
        } else {
            1.0
        };

        (work_area, dpi_scale)
    }
}

#[cfg(not(windows))]
pub fn primary_work_area_and_dpi() -> (Rect, f32) {
    (
        Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        },
        1.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottom_taskbar_positions_above_icon_and_clamps() {
        let work_area = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        };
        let icon = Rect {
            x: 1850,
            y: 1048,
            width: 24,
            height: 24,
        };
        let panel = Size {
            width: 340,
            height: 300,
        };

        assert_eq!(detect_taskbar_edge(icon, work_area), TaskbarEdge::Bottom);
        let pos = flyout_position(icon, work_area, panel);

        // Clamped at right edge: 1920 - 340 - 12 = 1568
        assert_eq!(pos.x, 1568);
        // Above icon: 1048 - 300 - 12 = 736; clamped to work_area max_y: 1040 - 300 - 12 = 728
        assert_eq!(pos.y, 728);
    }

    #[test]
    fn bottom_taskbar_centered_when_not_near_edge() {
        let work_area = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        };
        // Icon centered around x=960
        let icon = Rect {
            x: 948,
            y: 1045,
            width: 24,
            height: 24,
        };
        let panel = Size {
            width: 340,
            height: 300,
        };

        let pos = flyout_position(icon, work_area, panel);
        // icon_cx = 960, 960 - 170 = 790
        assert_eq!(pos.x, 790);
        assert_eq!(pos.y, 728);
    }

    #[test]
    fn top_taskbar_positions_below_icon() {
        let work_area = Rect {
            x: 0,
            y: 40,
            width: 1920,
            height: 1040,
        };
        let icon = Rect {
            x: 1850,
            y: 8,
            width: 24,
            height: 24,
        };
        let panel = Size {
            width: 340,
            height: 300,
        };

        assert_eq!(detect_taskbar_edge(icon, work_area), TaskbarEdge::Top);
        let pos = flyout_position(icon, work_area, panel);

        assert_eq!(pos.x, 1568);
        // Below icon: 8 + 24 + 12 = 44, clamped to min_y: 40 + 12 = 52
        assert_eq!(pos.y, 52);
    }

    #[test]
    fn right_taskbar_positions_left_of_icon() {
        let work_area = Rect {
            x: 0,
            y: 0,
            width: 1860,
            height: 1080,
        };
        let icon = Rect {
            x: 1870,
            y: 1000,
            width: 24,
            height: 24,
        };
        let panel = Size {
            width: 340,
            height: 300,
        };

        assert_eq!(detect_taskbar_edge(icon, work_area), TaskbarEdge::Right);
        let pos = flyout_position(icon, work_area, panel);

        // target_x = 1870 - 340 - 12 = 1518, clamped to max_x: 1860 - 340 - 12 = 1508
        assert_eq!(pos.x, 1508);
        // target_y = (1000 + 12) - 150 = 862, clamped to max_y: 1080 - 300 - 12 = 768
        assert_eq!(pos.y, 768);
    }

    #[test]
    fn left_taskbar_positions_right_of_icon() {
        let work_area = Rect {
            x: 60,
            y: 0,
            width: 1860,
            height: 1080,
        };
        let icon = Rect {
            x: 18,
            y: 1000,
            width: 24,
            height: 24,
        };
        let panel = Size {
            width: 340,
            height: 300,
        };

        assert_eq!(detect_taskbar_edge(icon, work_area), TaskbarEdge::Left);
        let pos = flyout_position(icon, work_area, panel);

        // target_x = 18 + 24 + 12 = 54, clamped to min_x: 60 + 12 = 72
        assert_eq!(pos.x, 72);
        assert_eq!(pos.y, 768);
    }

    #[test]
    fn fallback_position_near_bottom_right() {
        let work_area = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        };
        let panel = Size {
            width: 340,
            height: 300,
        };

        let pos = fallback_position(work_area, panel);
        assert_eq!(pos.x, 1920 - 340 - 12);
        assert_eq!(pos.y, 1040 - 300 - 12);
    }
}
