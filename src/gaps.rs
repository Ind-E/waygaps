use alloc::{string::String, vec::Vec};

use serde::Deserialize;

use crate::wayland;

#[derive(Debug, PartialEq)]
pub enum CornerEvent {
    Enter,
    Leave,
    Click(u32),
    // Scroll(Axis, f64),
}

#[derive(Clone, Debug, Deserialize)]
pub struct GapConfig {
    pub output: Option<String>,
    pub enter_command: Vec<String>,
    pub exit_command: Vec<String>,
    pub click_command: Vec<String>,
    pub anchor: Anchor,
    pub size: u32,
    pub margin: i32,
    pub timeout_ms: u16,
    pub color: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub enum Anchor {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
    Left,
    Right,
    Top,
    Bottom,
}

impl Default for Anchor {
    fn default() -> Self {
        Anchor::TopLeft
    }
}
