use alloc::collections::btree_map::BTreeMap;
use alloc::vec::Vec;
use core::ffi::{CStr, c_void};

use atoi::{FromRadix10Checked, FromRadix16Checked};
use memchr::{Memchr, memchr, memchr2, memrchr, memrchr2};
use rustix::fd::OwnedFd;
use rustix::fs::{self, Mode, OFlags};
use smallvec::SmallVec;
use wayland::wl_pointer::Axis;

use crate::log;
use crate::wayland::zwlr_layer_shell_v1;
use crate::{utils::getenv, wayland};

#[repr(u8)]
#[derive(PartialEq, Debug)]
pub enum InputEvent {
    Enter,
    Exit,
    Edge,
    Button(u32),
    Axis(Axis, i32),
}

#[derive(Debug)]
pub struct GapConfig {
    pub output: Option<&'static [u8]>,
    pub anchor: Anchor,
    pub size: u32,
    pub margin: i32,
    pub activation_force: u32,
    pub ignore_exclusive_zone: bool,
    pub layer: zwlr_layer_shell_v1::Layer,
    pub preview_color: Color,
    pub commands: SmallVec<[(InputEvent, &'static CStr); 8]>,
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
const fn default_activation_force() -> u32 {
    1000
}

#[inline]
const fn default_ignore_exclusive_zone() -> bool {
    true
}

#[inline]
const fn default_layer() -> zwlr_layer_shell_v1::Layer {
    zwlr_layer_shell_v1::Layer::overlay
}

#[inline]
const fn default_preview_color() -> Color {
    Color::new(25, 128, 16, 16)
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

impl Color {
    #[inline]
    pub const fn new(a: u8, r: u8, g: u8, b: u8) -> Self {
        Self { b, r, g, a }
    }

    #[inline]
    pub const fn as_u32(self) -> u32 {
        // SAFETY: this is safe because Color has the same size and alignment as
        // a u32
        unsafe { core::mem::transmute(self) }
    }
}

impl core::fmt::Display for Color {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:02}{:02}{:02}{:02}",
            self.r as u32 >> 24 & 0xFF,
            self.g as u32 >> 16 & 0xFF,
            self.b as u32 >> 8 & 0xFF,
            self.a as u32 & 0xFF,
        )
    }
}

#[derive(Debug)]
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
fn open_file(path: &CStr) -> OwnedFd {
    match fs::open(path, OFlags::RDONLY, Mode::empty()) {
        Ok(fd) => {
            log::info!("opening config file: {path:?}",);
            fd
        }
        Err(e) => {
            log::error!("error opening config file at {path:?}: {e}");
            origin::program::exit(1);
        }
    }
}

pub fn read_config(
    config_path: Option<&'static CStr>,
) -> BTreeMap<&'static str, GapConfig> {
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

        let path = unsafe { CStr::from_bytes_with_nul_unchecked(path_buf) };
        log::debug!("config path: {path:?}");
        open_file(path)
    };

    let len = match fs::fstat(&fd) {
        Ok(stat) => stat.st_size as usize,
        Err(e) => {
            log::error!("fstat failed: {e}");
            origin::program::exit(1);
        }
    };

    let ptr = core::ptr::null_mut::<c_void>();

    let mmap: &'static [u8] = unsafe {
        use rustix::mm;
        match mm::mmap(
            ptr,
            len,
            mm::ProtFlags::READ,
            mm::MapFlags::PRIVATE,
            fd,
            0,
        ) {
            Ok(mmap) => core::slice::from_raw_parts(mmap as *const u8, len),
            Err(e) => {
                log::error!("memmap failed: {e}");
                origin::program::exit(1);
            }
        }
    };

    // validate once, then use unsafe from_utf8_unchecked
    if let Err(e) = core::str::from_utf8(mmap) {
        log::error!("invalid UTF-8: {e}");
        origin::program::exit(1);
    }

    let mut gaps = Vec::new();

    parse_config(
        mmap,
        memchr::memchr_iter(b'\n', mmap),
        0,
        &mut gaps,
        0,
        Scope::Outer,
    );

    gaps.into_iter().collect()
}

enum Scope {
    Outer,
    Inner,
    Command,
}

fn parse_config(
    mmap: &'static [u8],
    mut newline_pos_iter: Memchr,
    line_start: usize,
    gaps: &mut Vec<(&'static str, GapConfig)>,
    current_gap: usize,
    scope: Scope,
) {
    let Some(newline_pos) = newline_pos_iter.next() else {
        log::trace!("no more newlines");
        return;
    };

    let line = trim_whitespace(&mmap[line_start..newline_pos]);

    if line.is_empty() || line.starts_with(b"//") {
        log::trace!("skipping blank line");
        return parse_config(
            mmap,
            newline_pos_iter,
            newline_pos + 1,
            gaps,
            current_gap,
            scope,
        );
    }

    match scope {
        Scope::Outer => {
            if memchr(b'{', line).is_some() {
                log::trace!("outer -> inner");
                let gap_name = utf8(before_first_whitespace(line));
                let gap_config = GapConfig::default();
                gaps.push((gap_name, gap_config));
                return parse_config(
                    mmap,
                    newline_pos_iter,
                    newline_pos + 1,
                    gaps,
                    current_gap,
                    Scope::Inner,
                );
            }
        }
        Scope::Inner => {
            if line == b"}" {
                log::trace!("inner -> outer");
                return parse_config(
                    mmap,
                    newline_pos_iter,
                    newline_pos + 1,
                    gaps,
                    current_gap + 1,
                    Scope::Outer,
                );
            }
            match before_first_whitespace(line) {
                b"output" => {
                    gaps[current_gap].1.output =
                        Some(trim_quotes(line));
                }
                b"anchor" => {
                    gaps[current_gap].1.anchor = parse_anchor(trim_quotes(line))
                }
                b"size" => {
                    gaps[current_gap].1.size = u32::from_radix_10_checked(
                        after_last_whitespace(line),
                    )
                    .0
                    .unwrap_or_else(|| {
                        log::warn!(
                            "could not parse size for {}, defaulting to {}",
                            gaps[current_gap].0,
                            default_size()
                        );
                        default_size()
                    })
                }
                b"margin" => gaps[current_gap].1.margin = atoi::atoi(
                    after_last_whitespace(line),
                )
                .unwrap_or_else(|| {
                    log::warn!(
                        "could not parse margin for {}, defaulting to {}",
                        gaps[current_gap].0,
                        default_margin()
                    );
                    default_margin()
                }),
                b"activation-force" => {
                    gaps[current_gap].1.activation_force = u32::from_radix_10_checked(
                        after_last_whitespace(line),
                    )
                    .0
                    .unwrap_or_else(|| {
                        log::warn!(
                            "could not parse activation force for {}, defaulting to {}",
                            gaps[current_gap].0,
                            default_activation_force()
                        );
                        default_activation_force()
                    })
                }
                b"ignore-exclusive-zone" => {
                    let ignore_exclusive_zone = match after_last_whitespace(line) {
                        b"true" => true,
                        b"false" => false,
                        _ => {
                            log::warn!(
                                "could not parse ignore exclusive zone for {}, defaulting to {}",
                                gaps[current_gap].0,
                                default_ignore_exclusive_zone()
                            );
                            default_ignore_exclusive_zone()
                        }
                    };

                    gaps[current_gap].1.ignore_exclusive_zone = ignore_exclusive_zone;
                }
                b"layer" => {
                    gaps[current_gap].1.layer = parse_layer(trim_quotes(line));
                }
                b"preview-color" => {
                    gaps[current_gap].1.preview_color = parse_preview_color(trim_quotes(line));
                }
                b"commands" => {
                    log::trace!("inner -> command");
                    return parse_config(
                        mmap,
                        newline_pos_iter,
                        newline_pos + 1,
                        gaps,
                        current_gap,
                        Scope::Command,
                    );
                }
                unknown => {
                    log::warn!("unknown key {}, ignoring", utf8(unknown));
                }
            }
            return parse_config(
                mmap,
                newline_pos_iter,
                newline_pos + 1,
                gaps,
                current_gap,
                scope,
            );
        }
        Scope::Command => {
            if line == b"}" {
                log::trace!("command -> inner");
                return parse_config(
                    mmap,
                    newline_pos_iter,
                    newline_pos + 1,
                    gaps,
                    current_gap,
                    Scope::Inner,
                );
            }
            let input = parse_command_input(before_first_whitespace(line));
            let command = trim_quotes_to_cstr(line);
            gaps[current_gap].1.commands.push((input, command));
            return parse_config(
                mmap,
                newline_pos_iter,
                newline_pos + 1,
                gaps,
                current_gap,
                scope,
            );
        }
    }
}

/// for other mouse buttons, use a tool like wev
///
/// Example - 272 is left mouse button
/// [     15:     wl_pointer] button: serial: 446213; time: 59602276; button:
/// 272 (left), state: 1 (pressed) [     15:     wl_pointer] frame
/// [     15:     wl_pointer] button: serial: 446214; time: 59602336; button:
/// 272 (left), state: 0 (released) [     15:     wl_pointer] frame
#[inline]
fn parse_command_input(key: &[u8]) -> InputEvent {
    match key {
        b"enter" => InputEvent::Enter,
        b"exit" | b"leave" => InputEvent::Exit,
        b"edge" => InputEvent::Edge,
        b"scroll-up" => InputEvent::Axis(Axis::vertical_scroll, -1),
        b"scroll-down" => InputEvent::Axis(Axis::vertical_scroll, 1),
        b"scroll-left" => InputEvent::Axis(Axis::horizontal_scroll, -1),
        b"scroll-right" => InputEvent::Axis(Axis::horizontal_scroll, 1),
        b"btn-left" => InputEvent::Button(272),
        b"btn-right" => InputEvent::Button(273),
        b"btn-middle" => InputEvent::Button(274),

        _ if key.starts_with(b"btn-") => {
            let num = &key[4..];
            let id = atoi::atoi(num).unwrap_or_else(|| {
                log::error!(
                    "invalid button id: '{}' in command '{}'",
                    utf8(key),
                    utf8(num)
                );
                origin::program::exit(1);
            });
            InputEvent::Button(id)
        }

        _ => {
            log::error!("unknown input event: {}", utf8(key));
            origin::program::exit(1);
        }
    }
}

#[inline]
fn utf8(data: &[u8]) -> &str {
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
    let retval = &s[start..end];
    log::trace!("trim: `{}` -> `{}`", utf8(s), utf8(retval));
    retval
}

#[inline]
fn trim_quotes(s: &[u8]) -> &[u8] {
    let start = memchr(b'"', s).unwrap_or(0);
    let end = memrchr(b'"', s).unwrap_or(s.len());
    let retval = &s[start + 1..end];
    log::trace!("trim_quotes: `{}` -> `{}`", utf8(s), utf8(retval));
    retval
}

#[inline]
fn trim_quotes_to_cstr(s: &[u8]) -> &CStr {
    let start = memchr(b'"', s).unwrap_or(0);
    let end = memrchr(b'"', s).unwrap_or(s.len() - 1);

    let inner = &s[start + 1..end];

    let mut buf = SmallVec::<[u8; 256]>::with_capacity(inner.len() + 1);
    buf.extend_from_slice(inner);
    buf.push(0);

    // TODO: I hate this
    let leaked: &'static [u8] = alloc::boxed::Box::leak(buf.into_boxed_slice());

    let retval = unsafe { CStr::from_bytes_with_nul_unchecked(leaked) };
    log::trace!("trim_quotes_to_cstr: `{}` -> `{:?}`", utf8(s), retval);
    retval
}

#[inline]
fn before_first_whitespace(s: &[u8]) -> &[u8] {
    if let Some(n) = memchr2(b' ', b'\t', s) {
        let retval = &s[0..n];
        log::trace!(
            "before_first_whitespace: `{}` -> `{}`",
            utf8(s),
            utf8(retval)
        );
        retval
    } else {
        log::trace!("failed to find in before_first_whitespace");
        s
    }
}

#[inline]
fn after_last_whitespace(s: &[u8]) -> &[u8] {
    if let Some(n) = memrchr2(b' ', b'\t', s) {
        let retval = &s[n + 1..];
        log::trace!(
            "after_last_whitespace: `{}` -> `{}`",
            utf8(s),
            utf8(retval)
        );
        retval
    } else {
        log::trace!("failed to find in after_last_whitespace");
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
                utf8(other),
                default_anchor()
            );
            default_anchor()
        }
    }
}

#[inline]
fn parse_layer(s: &[u8]) -> zwlr_layer_shell_v1::Layer {
    use zwlr_layer_shell_v1::Layer as wlrLayer;
    match s {
        b"overlay" => wlrLayer::overlay,
        b"top" => wlrLayer::top,
        b"bottom" => wlrLayer::bottom,
        b"background" => wlrLayer::background,
        other => {
            log::warn!(
                "{} is not a valid layer, defaulting to {}",
                utf8(other),
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
                utf8(s),
                default_preview_color()
            );
            let c = default_preview_color();
            return c;
        }
    };

    Color { r, g, b, a }
}
