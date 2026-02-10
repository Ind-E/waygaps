use atoi::{FromRadix10Checked as _, FromRadix16Checked as _};
use memchr::{memchr, memchr2, memrchr, memrchr2};
use rustix::fd::OwnedFd;
use rustix::fs::{self};
use smallvec::SmallVec;

use crate::gap::Color;
use crate::seat::Axis;
use crate::utils::getenv;
use crate::wayland::zwlr_layer_shell_v1;
use crate::{Config, log};

#[derive(PartialEq, Debug, Clone, Copy)]
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

#[repr(u8)]
#[derive(PartialEq, Debug, Clone, Copy)]
pub enum ScrollDir {
    Left,
    Right,
    Up,
    Down,
}

impl ScrollDir {
    #[inline]
    pub fn on_axis(self, axis: Axis) -> bool {
        matches!(self, ScrollDir::Left | ScrollDir::Right)
            && matches!(axis, Axis::Horizontal)
            || matches!(self, ScrollDir::Up | ScrollDir::Down)
                && matches!(axis, Axis::Vertical)
    }

    #[inline]
    pub fn is_positive(self) -> bool {
        matches!(self, ScrollDir::Right | ScrollDir::Down)
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
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

#[derive(Debug)]
pub struct GapConfig {
    pub output: Option<ArenaStr>,
    pub anchor: Anchor,
    pub size: u32,
    pub margin: i32,
    pub activation_force: u16,
    pub ignore_exclusive_zone: bool,
    pub layer: Layer,
    pub preview_color: Color,
    pub commands: SmallVec<[(InputEvent, ArenaStr); 4]>,
}

impl Default for GapConfig {
    fn default() -> Self {
        Self {
            output: Default::default(),
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
    Layer::Overlay
}

#[inline]
const fn default_preview_color() -> Color {
    Color::new(25, 128, 16, 16)
}

#[derive(Debug, Clone, Copy)]
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

#[inline(always)]
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

        let mut path_buf = [0u8; 512];
        let mut len = 0;

        for &b in home.to_bytes() {
            path_buf[len] = b;
            len += 1;
        }

        for &b in b"/.config/waygaps/config.kdl" {
            path_buf[len] = b;
            len += 1;
        }

        // null terminate
        path_buf[len] = 0;
        let path_buf = &path_buf[0..=len];

        let path =
            unsafe { core::ffi::CStr::from_bytes_with_nul_unchecked(path_buf) };
        open_file(path)
    };

    let len = match fs::fstat(&fd) {
        Ok(stat) => stat.st_size as usize,
        Err(e) => {
            log::error!("fstat failed: {e}");
            origin::program::exit(1);
        }
    };

    let empty_config = Config::new();

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

        let config = parse_config(mmap, empty_config);

        unsafe {
            let _ = rustix::mm::munmap(ptr, len);
        }

        config
    } else {
        let mut buffer = [0u8; 4096];

        let bytes_read = rustix::io::read(&fd, &mut buffer).unwrap();

        parse_config(&buffer[..bytes_read], empty_config)
    }
}

#[repr(u8)]
pub enum Scope {
    Outer,
    Inner,
    Command,
}

static mut STRING_ARENA: [u8; 4096] = [0u8; 4096];
static mut ARENA_OFFSET: usize = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct ArenaStr(u16);

impl ArenaStr {
    #[inline(always)]
    pub fn as_ptr(self) -> *const core::ffi::c_char {
        unsafe {
            let base = core::ptr::addr_of!(STRING_ARENA) as *const u8;
            base.add(self.0 as usize) as *const core::ffi::c_char
        }
    }

    #[inline(always)]
    pub fn as_cstr(self) -> &'static core::ffi::CStr {
        unsafe { core::ffi::CStr::from_ptr(self.as_ptr()) }
    }

    #[inline(always)]
    pub fn as_slice(self) -> &'static [u8] {
        unsafe {
            let base = self.as_ptr() as *const u8;
            let remaining_limit = 4096 - self.0 as usize;
            let len = memchr::memchr(
                0,
                core::slice::from_raw_parts(base, remaining_limit),
            )
            .unwrap_or(0);
            core::slice::from_raw_parts(base, len)
        }
    }

    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        unsafe { core::str::from_utf8_unchecked(self.as_slice()) }
    }
}

pub fn parse_config(data: &[u8], mut config: Config) -> Config {
    let mut line_start = 0;
    let mut cur_waygap = 0;
    let mut scope = Scope::Outer;

    for newline_pos in memchr::memchr_iter(b'\n', data) {
        let line = trim_whitespace(&data[line_start..newline_pos]);
        (scope, cur_waygap) =
            process_line(line, scope, &mut config, cur_waygap);
        line_start = newline_pos + 1;
    }
    if line_start < data.len() {
        process_line(&data[line_start..], scope, &mut config, cur_waygap);
    }
    return config.into();
}

fn process_line(
    line: &[u8],
    scope: Scope,
    config: &mut Config,
    cur_waygap: usize,
) -> (Scope, usize) {
    if line.is_empty() || line.starts_with(b"//") {
        return (scope, cur_waygap);
    }

    match scope {
        Scope::Outer => {
            if memchr(b'{', line).is_some() {
                let gap_name = arena_alloc_str(before_first_whitespace(line));
                let gap_config = GapConfig::default();
                config.push((gap_name, gap_config));
                return (Scope::Inner, cur_waygap);
            }
        }
        Scope::Inner => {
            if line == b"}" {
                return (Scope::Outer, cur_waygap + 1);
            }
            match before_first_whitespace(line) {
                b"output" => {
                    config[cur_waygap].1.output =
                        Some(arena_alloc_str(trim_quotes(line)));
                }
                b"anchor" => {
                    config[cur_waygap].1.anchor =
                        parse_anchor(trim_quotes(line))
                }
                b"size" => {
                    config[cur_waygap].1.size = parse_or(
                        u32::from_radix_10_checked(after_last_whitespace(line))
                            .0,
                        "size",
                        config[cur_waygap].0,
                        default_size,
                    )
                }
                b"margin" => {
                    config[cur_waygap].1.margin = parse_or(
                        atoi::atoi(after_last_whitespace(line)),
                        "margin",
                        config[cur_waygap].0,
                        default_margin,
                    )
                }
                b"activation-force" => {
                    config[cur_waygap].1.activation_force = parse_or(
                        u16::from_radix_10_checked(after_last_whitespace(line))
                            .0,
                        "activation-force",
                        config[cur_waygap].0,
                        default_activation_force,
                    )
                }
                b"ignore-exclusive-zone" => {
                    let ignore_exclusive_zone = match after_last_whitespace(
                        line,
                    ) {
                        b"true" => true,
                        b"false" => false,
                        _ => {
                            log::warn!(
                                "could not parse ignore-exclusive-zone for {}, defaulting to {}",
                                config[cur_waygap].0.as_str(),
                                default_ignore_exclusive_zone()
                            );
                            default_ignore_exclusive_zone()
                        }
                    };

                    config[cur_waygap].1.ignore_exclusive_zone =
                        ignore_exclusive_zone;
                }
                b"layer" => {
                    config[cur_waygap].1.layer =
                        parse_layer(trim_quotes(line));
                }
                b"preview-color" => {
                    config[cur_waygap].1.preview_color =
                        parse_preview_color(trim_quotes(line));
                }
                b"commands" => {
                    return (Scope::Command, cur_waygap);
                }
                unknown => {
                    log::warn!(
                        "unknown key {}, ignoring",
                        utf8_unsafe(unknown)
                    );
                }
            }
        }
        Scope::Command => {
            if line == b"}" {
                return (Scope::Inner, cur_waygap);
            }
            let input = parse_command_input(before_first_whitespace(line));
            let command = arena_alloc_str(trim_quotes(line));
            config[cur_waygap].1.commands.push((input, command));
        }
    }

    return (scope, cur_waygap);
}

#[inline]
fn arena_alloc_str(inner: &[u8]) -> ArenaStr {
    unsafe {
        let offset =
            core::ptr::read_volatile(core::ptr::addr_of!(ARENA_OFFSET));
        let len = inner.len() + 1;

        if offset + len > 4096 {
            origin::program::exit(1);
        }

        let arena_ptr = core::ptr::addr_of_mut!(STRING_ARENA) as *mut u8;
        let dest = arena_ptr.add(offset);

        core::ptr::copy_nonoverlapping(inner.as_ptr(), dest, inner.len());
        dest.add(inner.len()).write(0); // null terminate

        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(ARENA_OFFSET),
            offset + len,
        );

        ArenaStr(offset as u16)
    }
}

#[inline]
fn parse_command_input(key: &[u8]) -> InputEvent {
    use InputEvent::*;

    match key {
        b"enter" => Enter,
        b"leave" => Leave,
        b"edge" => Edge,
        b"scroll-up" => Scroll(ScrollDir::Up),
        b"scroll-down" => Scroll(ScrollDir::Down),
        b"scroll-left" => Scroll(ScrollDir::Left),
        b"scroll-right" => Scroll(ScrollDir::Right),
        b"mouse-left" => Button(272),
        b"mouse-right" => Button(273),
        b"mouse-middle" => Button(274),

        _ if key.starts_with(b"mouse-") => {
            let num = &key[6..];
            let id = atoi::atoi(num).unwrap_or_else(|| {
                log::error!(
                    "invalid button id: '{}' in command '{}'",
                    utf8_unsafe(key),
                    utf8_unsafe(num)
                );
                origin::program::exit(1);
            });
            InputEvent::Button(id)
        }

        _ => {
            log::error!("unknown input event: {}", utf8_unsafe(key));
            origin::program::exit(1);
        }
    }
}

#[inline]
fn utf8_unsafe(data: &[u8]) -> &str {
    unsafe { core::str::from_utf8_unchecked(data) }
}

#[inline]
fn trim_whitespace(s: &[u8]) -> &[u8] {
    let start = s
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(s.len());
    let end = s
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |x| x + 1);
    &s[start..end]
}

#[inline]
fn trim_quotes(s: &[u8]) -> &[u8] {
    let start = memchr(b'"', s).unwrap_or(0);
    let end = memrchr(b'"', s).unwrap_or(s.len());
    &s[start + 1..end]
}

#[inline]
fn before_first_whitespace(s: &[u8]) -> &[u8] {
    if let Some(n) = memchr2(b' ', b'\t', s) {
        &s[0..n]
    } else {
        log::warn!("failed to find in before_first_whitespace");
        s
    }
}

#[inline]
fn after_last_whitespace(s: &[u8]) -> &[u8] {
    if let Some(n) = memrchr2(b' ', b'\t', s) {
        &s[n + 1..]
    } else {
        log::warn!("failed to find in after_last_whitespace");
        s
    }
}

#[inline]
fn parse_anchor(s: &[u8]) -> Anchor {
    match s {
        b"left" => Anchor::Left,
        b"right" => Anchor::Right,
        b"top" => Anchor::Top,
        b"bottom" => Anchor::Bottom,
        b"top-left" => Anchor::TopLeft,
        b"top-right" => Anchor::TopRight,
        b"bottom-left" => Anchor::BottomLeft,
        b"bottom-right" => Anchor::BottomRight,
        other => {
            log::warn!(
                "{} is not a valid anchor, defaulting to {}",
                utf8_unsafe(other),
                default_anchor()
            );
            default_anchor()
        }
    }
}

#[inline]
fn parse_layer(s: &[u8]) -> Layer {
    match s {
        b"overlay" => Layer::Overlay,
        b"top" => Layer::Top,
        b"bottom" => Layer::Bottom,
        b"background" => Layer::Background,
        other => {
            log::warn!(
                "{} is not a valid layer, defaulting to {}",
                utf8_unsafe(other),
                default_layer()
            );
            default_layer()
        }
    }
}

#[inline]
fn parse_preview_color(s: &[u8]) -> Color {
    let (n, digits) = u32::from_radix_16_checked(s);

    let (r, g, b, a) = match (n, digits) {
        // RRGGBB
        (Some(n), 6) => (
            ((n >> 16) & 0xFF) as u8,
            ((n >> 8) & 0xFF) as u8,
            (n & 0xFF) as u8,
            0xFF,
        ),

        // RRGGBBAA
        (Some(n), 8) => (
            ((n >> 24) & 0xFF) as u8,
            ((n >> 16) & 0xFF) as u8,
            ((n >> 8) & 0xFF) as u8,
            (n & 0xFF) as u8,
        ),

        _ => {
            log::warn!(
                "{} is not a valid debug color, defaulting to {}",
                utf8_unsafe(s),
                default_preview_color()
            );
            let c = default_preview_color();
            return c;
        }
    };

    Color { r, g, b, a }
}

#[inline]
fn parse_or<T: Copy + core::fmt::Display>(
    value: Option<T>,
    name: &str,
    gap: ArenaStr,
    default: fn() -> T,
) -> T {
    value.unwrap_or_else(|| {
        let d = default();
        log::warn!(
            "could not parse {} for {}, defaulting to {}",
            name,
            gap.as_str(),
            d
        );
        d
    })
}
