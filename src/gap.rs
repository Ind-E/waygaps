use core::ffi::CStr;

use rustix::{self, fd::OwnedFd};
use waybackend::{
    Waybackend, objman::ObjectManager, shm::MmappedSlice, types::ObjectId,
};
use wayland::zwlr_layer_surface_v1::Anchor as wlrAnchor;

use crate::{
    BUFFER_SCALE,
    config::{Anchor, Color, GapConfig, InputEvent},
    wayland,
};

const NAMESPACE: &str = "waygaps";

pub struct WayGap {
    pub registry_name: u32,

    pub wl_surface: ObjectId,
    pub layer_surface: ObjectId,
    pub wl_output: ObjectId,

    pub width: u32,
    pub height: u32,
    pub anchor: Anchor,

    bufsize: u32,

    shm: OwnedFd,

    pub configured: bool,
    pub redraw: bool,

    pub commands: &'static [(InputEvent, &'static CStr)],
    pub preview_color: Color,
    pub activation_force: u32,
}

impl WayGap {
    pub fn new(
        backend: &mut Waybackend,
        objman: &mut ObjectManager<WaylandObject>,
        registry_name: u32,
        wl_compositor: ObjectId,
        layer_shell: ObjectId,
        wl_output: ObjectId,
        config: &GapConfig,
    ) -> rustix::io::Result<Self> {
        let wl_surface = objman.create(WaylandObject::Surface);
        wayland::wl_compositor::req::create_surface(
            backend,
            wl_compositor,
            wl_surface,
        )?;

        let layer_surface = objman.create(WaylandObject::LayerSurface);
        wayland::zwlr_layer_shell_v1::req::get_layer_surface(
            backend,
            layer_shell,
            layer_surface,
            wl_surface,
            Some(wl_output),
            wayland::zwlr_layer_shell_v1::Layer::overlay,
            NAMESPACE,
        )?;

        let size = config.size * BUFFER_SCALE;

        let (anchor, width, height) = match config.anchor {
            Anchor::TopLeft => (wlrAnchor::LEFT | wlrAnchor::TOP, size, size),
            Anchor::TopRight => (wlrAnchor::RIGHT | wlrAnchor::TOP, size, size),
            Anchor::BottomRight => {
                (wlrAnchor::RIGHT | wlrAnchor::BOTTOM, size, size)
            }
            Anchor::BottomLeft => {
                (wlrAnchor::LEFT | wlrAnchor::BOTTOM, size, size)
            }
            Anchor::Left => (
                wlrAnchor::LEFT | wlrAnchor::TOP | wlrAnchor::BOTTOM,
                size,
                0,
            ),
            Anchor::Right => (
                wlrAnchor::RIGHT | wlrAnchor::TOP | wlrAnchor::BOTTOM,
                size,
                0,
            ),
            Anchor::Top => {
                (wlrAnchor::TOP | wlrAnchor::LEFT | wlrAnchor::RIGHT, 0, size)
            }
            Anchor::Bottom => (
                wlrAnchor::BOTTOM | wlrAnchor::LEFT | wlrAnchor::RIGHT,
                0,
                size,
            ),
        };

        if config.ignore_exclusive_zone {
            wayland::zwlr_layer_surface_v1::req::set_exclusive_zone(
                backend,
                layer_surface,
                -1,
            )?;
        }

        let (top, right, bottom, left) = match config.anchor {
            Anchor::Left | Anchor::Right => {
                (config.margin, 0, config.margin, 0)
            }
            Anchor::Top | Anchor::Bottom => {
                (0, config.margin, 0, config.margin)
            }
            _ => (0, 0, 0, 0),
        };

        wayland::zwlr_layer_surface_v1::req::set_margin(
            backend,
            layer_surface,
            top,
            right,
            bottom,
            left,
        )?;

        wayland::zwlr_layer_surface_v1::req::set_layer(
            backend,
            layer_surface,
            config.layer,
        )?;

        wayland::zwlr_layer_surface_v1::req::set_anchor(
            backend,
            layer_surface,
            anchor,
        )?;
        wayland::zwlr_layer_surface_v1::req::set_size(
            backend,
            layer_surface,
            width,
            height,
        )?;

        wayland::wl_surface::req::commit(backend, wl_surface)?;

        Ok(Self {
            registry_name,

            wl_surface,
            layer_surface,
            wl_output,

            width,
            height,
            anchor: config.anchor,

            bufsize: 0,

            shm: waybackend::shm::create()?,

            configured: false,
            redraw: false,

            commands: unsafe {
                core::mem::transmute(config.commands.as_slice())
            },
            preview_color: config.preview_color,
            activation_force: config.activation_force,
        })
    }

    pub fn draw_frame(
        &mut self,
        backend: &mut Waybackend,
        wayland_shm: ObjectId,
        objman: &mut ObjectManager<WaylandObject>,
    ) -> rustix::io::Result<()> {
        let pool = objman.create(WaylandObject::ShmPool);
        let buffer = objman.create(WaylandObject::Buffer);

        wayland::wl_shm::req::create_pool(
            backend,
            wayland_shm,
            pool,
            &self.shm,
            self.bufsize as i32 * 4,
        )?;
        wayland::wl_shm_pool::req::create_buffer(
            backend,
            pool,
            buffer,
            0,
            self.width as i32,
            self.height as i32,
            self.width as i32 * 4,
            wayland::wl_shm::Format::argb8888,
        )?;

        wayland::wl_shm_pool::req::destroy(backend, pool)?;

        wayland::wl_surface::req::set_buffer_scale(
            backend,
            self.wl_surface,
            super::BUFFER_SCALE as i32,
        )?;
        wayland::wl_surface::req::attach(
            backend,
            self.wl_surface,
            Some(buffer),
            0,
            0,
        )?;
        wayland::wl_surface::req::damage_buffer(
            backend,
            self.wl_surface,
            0,
            0,
            self.width as i32,
            self.height as i32,
        )?;
        wayland::wl_surface::req::commit(backend, self.wl_surface)?;
        self.redraw = false;
        Ok(())
    }

    /// Size is given in pixels
    #[inline]
    pub fn expand_shm(&mut self, size: u32) {
        if size * 4 > self.bufsize {
            rustix::io::retry_on_intr(|| {
                rustix::fs::ftruncate(&self.shm, size as u64 * 4)
            })
            .expect("failed to truncate shm file");
            self.bufsize = size * 4;
        }
    }

    #[inline]
    pub fn draw_preview(&mut self) {
        let mmap =
            MmappedSlice::new(&mut self.shm, self.bufsize as usize, 0).unwrap();

        bg(
            mmap.0,
            0,
            0,
            self.width,
            self.height,
            self.width, // stride
            self.preview_color,
        );
    }
}

#[inline]
pub fn bg(
    canvas: &mut [u32],
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    stride: u32,
    color: Color,
) {
    assert!(
        (((y2 - 1) * stride + (x2 - 1)) as usize) < canvas.len(),
        "final index: {} length: {}",
        ((y2 - 1) * stride + (x2 - 1)) as usize,
        canvas.len()
    );
    let color = color.as_u32();

    // SAFETY: we just verified these bounds above
    for y in y1..y2 {
        // by declaring this row here like this, we make it easier for the
        // compiler to vectorize this code
        let row = unsafe { canvas.as_mut_ptr().add((y * stride) as usize) };
        for x in x1..x2 {
            unsafe { row.add(x as usize).write(color) }
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq)]
pub enum WaylandObject {
    // standard stuff
    Display,
    Registry,
    Callback,
    Compositor,
    Seat,
    Shm,
    ShmPool,
    Buffer,
    Surface,
    Output,
    Pointer,

    // layer shell
    LayerShell,
    LayerSurface,

    // relative pointer
    RelativePointer,
    RelativePointerMgr,
}
