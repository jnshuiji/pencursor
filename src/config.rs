//! 光标配置与样式定义模块

/// 手写笔悬空时的光标呈现样式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)]
pub enum CursorStyle {
    /// 高对比度十字准星（带中心空隙）
    Crosshair,
    /// 高对比度笔刷圆环（带中心微点）
    Circle,
    /// 十字准星与笔刷圆环复合显示
    #[default]
    Both,
}

/// 应用程序全局配置
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Config {
    /// 悬空光标样式
    pub style: CursorStyle,

    /// 十字准星臂长（像素）
    pub crosshair_size: f32,
    /// 十字准星中心间隙半径（像素）
    pub crosshair_gap: f32,
    /// 十字准星主线条粗细（像素）
    pub crosshair_thickness: f32,

    /// 圆环光标半径（像素）
    pub circle_radius: f32,
    /// 圆环线条粗细（像素）
    pub circle_thickness: f32,

    /// 笔尖触屏接触时微缩点的半径（像素）
    pub contact_dot_radius: f32,

    /// 判定手写笔悬空离线的超时阈值（毫秒）
    pub hover_timeout_ms: u64,
    /// 鼠标移动后恢复手写笔监听的静默超时阈值（毫秒）
    pub mouse_inactivity_timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            style: CursorStyle::Both,
            crosshair_size: 10.0,
            crosshair_gap: 3.5,
            crosshair_thickness: 1.5,
            circle_radius: 12.0,
            circle_thickness: 1.5,
            contact_dot_radius: 2.0,
            hover_timeout_ms: 150,
            mouse_inactivity_timeout_ms: 100,
        }
    }
}
