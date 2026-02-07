use alloc::vec;
use alloc::{
    boxed::Box, collections::btree_map::BTreeMap, ffi::CString, format,
    string::String, vec::Vec,
};
use core::ffi::CStr;

use rustc_hash::FxBuildHasher;
use rustix::fs::{self, Mode, OFlags};
use rustix::path::Arg as _;
use serde::Deserialize;
use tracing::warn;
use wayland::wl_pointer::Axis;

use crate::{utils::getenv, wayland};

pub type FxHashMap<K, V> = hashbrown::HashMap<K, V, FxBuildHasher>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum InputEvent {
    Enter,
    Exit,
    Button(u32),
    Axis(Axis, i32),
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct GapConfigRaw {
    pub output: Option<String>,
    pub commands: BTreeMap<String, String>,
    pub anchor: Option<Anchor>,
    pub size: Option<u32>,
    pub margin: Option<i32>,
    pub activation_force: Option<u32>,
    pub color: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct GapConfig {
    pub output: Option<String>,
    pub commands: FxHashMap<InputEvent, Box<CStr>>,
    pub anchor: Anchor,
    pub size: u32,
    pub margin: i32,
    pub activation_force: u32,
    pub color: Color,
}

#[repr(C, align(4))]
/// Color representation in BGRA in native endian.
/// Can be safely transmuted into a u32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub b: u8,
    pub g: u8,
    pub r: u8,
    pub a: u8,
}

impl Default for Color {
    fn default() -> Self {
        Self {
            r: 192,
            g: 16,
            b: 16,
            a: 128,
        }
    }
}

impl Color {
    #[inline]
    pub const fn new(a: u8, r: u8, g: u8, b: u8) -> Self {
        Self { b, r, g, a }
    }

    #[inline]
    pub const fn new_from_u32(x: u32) -> Self {
        // SAFETY: this is safe because Color has the same size and alignment as a u32
        unsafe { core::mem::transmute(x) }
    }

    #[inline]
    pub const fn new_from_rgba_u32(x: u32) -> Self {
        Self::new_from_u32(x.rotate_right(8))
    }

    #[inline]
    pub const fn as_u32(self) -> u32 {
        // SAFETY: this is safe because Color has the same size and alignment as a u32
        unsafe { core::mem::transmute(self) }
    }
}

#[derive(Clone, Debug, Deserialize, Default)]
pub enum Anchor {
    #[default]
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
    Left,
    Right,
    Top,
    Bottom,
}

impl<'de> Deserialize<'de> for GapConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = GapConfigRaw::deserialize(deserializer)?;

        let mut commands = FxHashMap::default();

        for (key, cmd) in raw.commands {
            let event = parse_input_key(&key)
                .map_err(|e| serde::de::Error::custom(e))?;
            let cstr = CString::new(cmd)
                .map_err(|_| {
                    serde::de::Error::custom("command contains NUL byte")
                })?
                .into_boxed_c_str();

            commands.insert(event, cstr);
        }

        Ok(GapConfig {
            output: raw.output,
            commands,
            anchor: raw.anchor.unwrap_or(Anchor::Left),
            size: raw.size.unwrap_or(25),
            margin: raw.margin.unwrap_or(0),
            activation_force: raw.activation_force.unwrap_or(0),
            color: Color::new_from_u32(raw.color.unwrap_or(0)),
        })
    }
}

/// for other mouse buttons, use a tool like wev
///
/// Example - 272 is left mouse button
/// [     15:     wl_pointer] button: serial: 446213; time: 59602276; button: 272 (left), state: 1 (pressed)
/// [     15:     wl_pointer] frame
/// [     15:     wl_pointer] button: serial: 446214; time: 59602336; button: 272 (left), state: 0 (released)
/// [     15:     wl_pointer] frame
fn parse_input_key(key: &str) -> Result<InputEvent, String> {
    match key {
        "enter" => Ok(InputEvent::Enter),
        "exit" | "leave" => Ok(InputEvent::Exit),
        "scroll_up" => Ok(InputEvent::Axis(Axis::vertical_scroll, 1)),
        "scroll_down" => Ok(InputEvent::Axis(Axis::vertical_scroll, -1)),
        "scroll_left" => Ok(InputEvent::Axis(Axis::horizontal_scroll, -1)),
        "scroll_right" => Ok(InputEvent::Axis(Axis::horizontal_scroll, 1)),
        "btn_left" => Ok(InputEvent::Button(input_event_codes::BTN_LEFT!())),
        "btn_right" => Ok(InputEvent::Button(input_event_codes::BTN_RIGHT!())),
        "btn_middle" => {
            Ok(InputEvent::Button(input_event_codes::BTN_MIDDLE!()))
        }

        _ if key.starts_with("btn_") => {
            let num = &key[4..];
            let id = num.parse::<u32>().map_err(|_| {
                format!("invalid button id: '{key}' in command '{num}'")
            })?;
            Ok(InputEvent::Button(id))
        }

        _ => Err(format!("unknown input event: {key}")),
    }
}

pub fn read_config_file_to_string() -> Result<String, &'static str> {
    let home = unsafe {
        getenv(c"HOME").unwrap_or_else(|| {
        warn!("HOME environment variable is not set, searching for config in current directory");
        c"."
    })
    };
    let home = home.as_str().map_err(|_| "HOME is not valid UTF-8")?;

    let path = format!("{}/.config/waygaps/config.toml", home);

    let c_path = CString::new(path).map_err(|_| "invalid path")?;

    let fd = fs::open(&c_path, OFlags::RDONLY, Mode::empty())
        .map_err(|_| "Could not open config file")?;

    let mut buf = vec![0u8; 8192];
    let mut total_read = 0;

    loop {
        let n = rustix::io::read(&fd, &mut buf[total_read..])
            .map_err(|_| "Error reading file")?;
        if n == 0 {
            break;
        } // EOF
        total_read += n;
        if total_read == buf.len() {
            buf.resize(buf.len() * 2, 0);
        }
    }
    buf.truncate(total_read);

    String::from_utf8(buf).map_err(|_| "File is not valid UTF-8")
}

pub fn load_config() -> Result<BTreeMap<String, GapConfig>, &'static str> {
    let toml_str = read_config_file_to_string()?;

    let config =
        toml::from_str(&toml_str).map_err(|_| "Failed to parse TOML")?;

    Ok(config)
}
