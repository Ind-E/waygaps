use alloc::boxed::Box;
use alloc::rc::Rc;

use rustix::fd::OwnedFd;
use rustix::fs::{self};
use serde::Deserialize;

use crate::gap::Color;
use crate::log;
use crate::seat::Axis;
use crate::utils::getenv;
use crate::wayland::zwlr_layer_shell_v1;

pub struct Config(Box<[(Box<str>, GapConfig)]>);

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
pub struct BTreeConfig(
    alloc::collections::btree_map::BTreeMap<Box<str>, GapConfig>,
);

impl core::ops::Deref for Config {
    type Target = Box<[(Box<str>, GapConfig)]>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for Config {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(PartialEq, Debug, Clone, Copy, Deserialize)]
#[serde(try_from = "Box<str>")]
pub enum InputEvent {
    Enter,
    Leave,
    Edge,
    // This can be a u16 instead of a u32 because button codes
    // above 0xFFFF are currently undefined (but may be used in future
    // versions of the wl_pointer protocol)
    Button(u16),
    Scroll(ScrollDir),
}

impl TryFrom<Box<str>> for InputEvent {
    type Error = alloc::string::String;

    fn try_from(key: Box<str>) -> Result<Self, Self::Error> {
        use InputEvent::*;

        match key.as_ref() {
            "enter" => Ok(Enter),
            "leave" => Ok(Leave),
            "edge" => Ok(Edge),
            "scroll-up" => Ok(Scroll(ScrollDir::Up)),
            "scroll-down" => Ok(Scroll(ScrollDir::Down)),
            "scroll-left" => Ok(Scroll(ScrollDir::Left)),
            "scroll-right" => Ok(Scroll(ScrollDir::Right)),
            "mouse-left" => Ok(Button(272)),
            "mouse-right" => Ok(Button(273)),
            "mouse-middle" => Ok(Button(274)),

            _ if key.starts_with("mouse-") => {
                let num = &key[6..];
                let id = match num.parse::<u16>() {
                    Ok(id) => id,
                    Err(e) => {
                        return Err(alloc::format!(
                            "invalid mouse button id: '{key}' in command '{num}': {e}"
                        ));
                    }
                };
                Ok(InputEvent::Button(id))
            }

            _ => Err(alloc::format!(
                "unknown input event: `{key}`, expected one of `enter`, `leave`, `edge`, `scroll-up`, `scroll-down`, `scroll-left`, `scroll-right`, `mouse-left`, `mouse-right`, or `mouse-middle`"
            )),
        }
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for InputEvent {
    fn schema_name() -> alloc::borrow::Cow<'static, str> {
        "InputEvent".into()
    }

    fn schema_id() -> alloc::borrow::Cow<'static, str> {
        concat!(module_path!(), "InputEvent").into()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "anyOf": [
                {
                    "type": "string",
                    "enum": [
                        "enter",
                        "leave",
                        "edge",
                        "scroll-up",
                        "scroll-down",
                        "scroll-left",
                        "scroll-right",
                        "mouse-left",
                        "mouse-right",
                        "mouse-middle"
                    ]
                },
                {
                    "type": "string",
                    "pattern": "^mouse-\\d+$"
                }
            ]
        })
    }
}

#[repr(u8)]
#[derive(PartialEq, Eq, Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ScrollDir {
    Left,
    Right,
    Up,
    Down,
}

impl ScrollDir {
    #[inline]
    pub const fn on_axis(self, axis: Axis) -> bool {
        matches!(self, ScrollDir::Left | ScrollDir::Right)
            && matches!(axis, Axis::Horizontal)
            || matches!(self, ScrollDir::Up | ScrollDir::Down)
                && matches!(axis, Axis::Vertical)
    }

    #[inline]
    pub const fn is_positive(self) -> bool {
        matches!(self, ScrollDir::Right | ScrollDir::Down)
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[repr(u8)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Layer {
    Background,
    Bottom,
    Top,
    Overlay,
}

impl core::fmt::Display for Layer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Layer::Overlay => "overlay",
                Layer::Top => "top",
                Layer::Bottom => "bottom",
                Layer::Background => "background",
            }
        )
    }
}
use zwlr_layer_shell_v1::Layer as wlrLayer;

impl From<Layer> for wlrLayer {
    #[inline]
    fn from(value: Layer) -> Self {
        // SAFETY: wlrLayer has repr(u32) and has variants in
        // the same order
        unsafe { core::mem::transmute(value as u32) }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GapConfig {
    pub output: Option<Box<str>>,
    pub anchor: Anchor,
    pub size: u32,
    pub margin: i32,
    pub activation_force: u16,
    pub ignore_exclusive_zone: bool,
    pub layer: Layer,
    pub preview_color: Color,

    #[cfg(feature = "schema")]
    pub commands: Rc<[(InputEvent, Box<str>)]>,
    #[cfg(not(feature = "schema"))]
    pub commands: Rc<[(InputEvent, Box<core::ffi::CStr>)]>,
}

impl Default for GapConfig {
    fn default() -> Self {
        Self {
            output: Option::default(),
            anchor: default_anchor(),
            size: default_size(),
            margin: default_margin(),
            activation_force: default_activation_force(),
            ignore_exclusive_zone: default_ignore_exclusive_zone(),
            layer: default_layer(),
            preview_color: default_preview_color(),
            commands: Default::default(),
        }
    }
}

#[inline]
const fn default_anchor() -> Anchor {
    Anchor::TopLeft
}

#[inline]
const fn default_size() -> u32 {
    10
}

#[inline]
const fn default_margin() -> i32 {
    0
}

#[inline]
const fn default_activation_force() -> u16 {
    25
}

#[inline]
const fn default_ignore_exclusive_zone() -> bool {
    true
}

#[inline]
const fn default_layer() -> Layer {
    Layer::Top
}

#[inline]
const fn default_preview_color() -> Color {
    Color::new(25, 128, 16, 16)
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

impl core::fmt::Display for Anchor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Anchor::TopLeft => "top-left",
                Anchor::TopRight => "top-right",
                Anchor::BottomRight => "bottom-right",
                Anchor::BottomLeft => "bottom-left",
                Anchor::Left => "left",
                Anchor::Right => "right",
                Anchor::Top => "top",
                Anchor::Bottom => "bottom",
            }
        )
    }
}

impl core::fmt::Display for zwlr_layer_shell_v1::Layer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        use zwlr_layer_shell_v1::Layer as wlrLayer;
        write!(
            f,
            "{}",
            match self {
                wlrLayer::overlay => "overlay",
                wlrLayer::top => "top",
                wlrLayer::bottom => "bottom",
                wlrLayer::background => "background",
            }
        )
    }
}

fn open_file(path: &core::ffi::CStr) -> OwnedFd {
    match fs::open(path, fs::OFlags::RDONLY, fs::Mode::empty()) {
        Ok(fd) => {
            log::info!("opening config file: `{}`", path.display());
            fd
        }
        Err(e) => {
            log::error!(
                "error opening config file at `{}`: {e}",
                path.display()
            );
            origin::program::exit(1);
        }
    }
}

#[inline]
pub fn read_config(config_path: Option<&'static core::ffi::CStr>) -> Config {
    let fd = if let Some(path) = config_path {
        open_file(path)
    } else {
        let home = unsafe {
            getenv(b"HOME").unwrap_or_else(|| {
                log::warn!("HOME environment variable is not set, searching for config in current directory");
                c"."
            })
        };

        let home_bytes = home.to_bytes();
        let suffix = b"/.config/waygaps/config.toml";

        let mut path_buf =
            alloc::vec::Vec::with_capacity(home_bytes.len() + suffix.len() + 1);

        path_buf.extend_from_slice(home_bytes);
        path_buf.extend_from_slice(suffix);
        path_buf.push(0); // null terminate

        let path = unsafe {
            core::ffi::CStr::from_bytes_with_nul_unchecked(&path_buf)
        };

        open_file(path)
    };

    let len = match fs::fstat(&fd) {
        Ok(stat) => stat.st_size as usize,
        Err(e) => {
            log::error!("fstat failed: {e}");
            origin::program::exit(1);
        }
    };

    if len > 4096 {
        log::warn!("config file is large, using mmap");
        let ptr = unsafe {
            use rustix::mm;
            match mm::mmap(
                core::ptr::null_mut(),
                len,
                mm::ProtFlags::READ,
                mm::MapFlags::PRIVATE,
                fd,
                0,
            ) {
                Ok(mmap_ptr) => mmap_ptr,
                Err(e) => {
                    log::error!("memmap failed: {e}");
                    origin::program::exit(1);
                }
            }
        };

        let mmap =
            unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };

        let toml_str = match str::from_utf8(mmap) {
            Ok(s) => s,
            Err(e) => {
                log::error!("Config file is not valid UTF-8: {e}");
                origin::program::exit(1);
            }
        };

        let config = match toml::from_str::<
            alloc::collections::BTreeMap<Box<str>, GapConfig>,
        >(toml_str)
        {
            Ok(config) => Config(config.into_iter().collect()),
            Err(e) => {
                log::error!("Failed to parse TOML config:\n{e}");
                unsafe {
                    let _ = rustix::mm::munmap(ptr, len);
                }
                origin::program::exit(1);
            }
        };

        unsafe {
            let _ = rustix::mm::munmap(ptr, len);
        }

        config
    } else {
        let mut buffer = [0u8; 4096];

        let bytes_read = match rustix::io::read(&fd, &mut buffer) {
            Ok(b) => b,
            Err(e) => {
                log::error!("Failed to read config file: {e}");
                origin::program::exit(1);
            }
        };

        let toml_str = match str::from_utf8(&buffer[..bytes_read]) {
            Ok(s) => s,
            Err(e) => {
                log::error!("Config file is not valid UTF-8: {e}");
                origin::program::exit(1);
            }
        };

        match toml::from_str::<BTreeConfig>(toml_str) {
            Ok(config) => Config(config.0.into_iter().collect()),
            Err(e) => {
                log::error!("Failed to parse TOML config:\n{e}");
                origin::program::exit(1);
            }
        }
    }
}
