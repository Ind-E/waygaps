use alloc::{boxed::Box, collections::btree_map::BTreeMap, vec::Vec};
use core::ffi::CStr;

use rustix::{self, fd::OwnedFd};
use waybackend::{
    Waybackend, objman::ObjectManager, shm::MmappedSlice, types::ObjectId,
};
use wayland::zwlr_layer_surface_v1::Anchor as wlrAnchor;

use crate::{
    config::{Anchor, Color, FxHashMap, GapConfig, InputEvent},
    wayland,
};

const NAMESPACE: &str = "waygaps";

pub struct WayGap {
    pub registry_name: u32,

    pub wl_output: ObjectId,
    pub wl_surface: ObjectId,
    pub layer_surface: ObjectId,

    pub frame_callback: Option<ObjectId>,

    pub width: u32,
    pub height: u32,
    pub anchor: wlrAnchor,

    bufsize: u32,
    pub ftags: u32,
    pub otags: u32,
    pub urg: u32,

    shm: OwnedFd,

    pub configured: bool,
    pub selected: bool,
    pub redraw: bool,

    last_window_width: u32,

    pub commands: FxHashMap<InputEvent, Box<CStr>>,
    pub color: Color,
    pub margin: i32,
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
        config: GapConfig,
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

        let (anchor, width, height) = match config.anchor {
            Anchor::TopLeft => {
                (wlrAnchor::LEFT | wlrAnchor::TOP, config.size, config.size)
            }
            Anchor::TopRight => {
                (wlrAnchor::RIGHT | wlrAnchor::TOP, config.size, config.size)
            }
            Anchor::BottomRight => (
                wlrAnchor::RIGHT | wlrAnchor::BOTTOM,
                config.size,
                config.size,
            ),
            Anchor::BottomLeft => (
                wlrAnchor::LEFT | wlrAnchor::BOTTOM,
                config.size,
                config.size,
            ),
            Anchor::Left => (wlrAnchor::LEFT, config.size, 0),
            Anchor::Right => (wlrAnchor::RIGHT, config.size, 0),
            Anchor::Top => (wlrAnchor::TOP, 0, config.size),
            Anchor::Bottom => (wlrAnchor::BOTTOM, 0, config.size),
        };

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

        Ok(Self {
            registry_name,

            wl_output,
            wl_surface,
            layer_surface,

            frame_callback: None,

            width,
            height,
            anchor,

            bufsize: 0,
            ftags: 0,
            otags: 0,
            urg: 0,

            shm: waybackend::shm::create()?,

            configured: false,
            selected: false,
            redraw: false,

            last_window_width: 0,
            commands: config.commands,
            color: config.color,
            activation_force: config.activation_force,
            margin: config.margin,
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
        let frame = objman.create(WaylandObject::Callback);

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
        wayland::wl_surface::req::frame(backend, self.wl_surface, frame)?;
        wayland::wl_surface::req::commit(backend, self.wl_surface)?;
        self.redraw = false;
        self.frame_callback = Some(frame);
        Ok(())
    }

    /// Size is given in pixels
    pub fn expand_shm(&mut self, size: u32) {
        if size * 4 > self.bufsize {
            rustix::io::retry_on_intr(|| {
                rustix::fs::ftruncate(&self.shm, size as u64 * 4)
            })
            .expect("failed to truncate shm file");
            self.bufsize = size * 4;
        }
    }

    pub fn draw_debug(&mut self) {
        let mmap =
            MmappedSlice::new(&mut self.shm, self.bufsize as usize, 0).unwrap();

        let stride = self.width;

        let (x1, y1, x2, y2) = (0, 0, self.width, self.height);
        // match self.anchor {
        //     // left edge
        //     wlrAnchor::LEFT | wlrAnchor::BOTTOM | wlrAnchor::TOP => {
        //         (0, 0, self.width, self.height)
        //     }
        //     // right edge
        //     wlrAnchor::RIGHT | wlrAnchor::BOTTOM | wlrAnchor::TOP => {
        //
        //     }
        //     // top edge
        //     wlrAnchor::TOP | wlrAnchor::LEFT | wlrAnchor::RIGHT => {}
        //     // bottom edge
        //     wlrAnchor::BOTTOM | wlrAnchor::LEFT | wlrAnchor::RIGHT => {}
        //     // top left corner
        //     wlrAnchor::TOP | wlrAnchor::LEFT => {}
        //     // top right corner
        //     wlrAnchor::TOP | wlrAnchor::RIGHT => {}
        //     // bottom left corner
        //     wlrAnchor::BOTTOM | wlrAnchor::LEFT => {}
        //     // bottom right corner
        //     wlrAnchor::BOTTOM | wlrAnchor::RIGHT => {}
        //     _ => unreachable!(),
        // };

        bg(mmap.0, x1, y1, x2, y2, stride, self.color);
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

    //SAFETY: we just verified these bounds above
    for y in y1..y2 {
        // by declaring this row here like this, we make it easier for the compiler to
        // vectorize this code
        let row = unsafe { canvas.as_mut_ptr().add((y * stride) as usize) };
        for x in x1..x2 {
            unsafe { row.add(x as usize).write(color) }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
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
}
