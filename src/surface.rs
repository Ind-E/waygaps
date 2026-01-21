use wayland::zwlr_layer_surface_v1::Anchor as wlrAnchor;

use waybackend::{Waybackend, objman::ObjectManager, types::ObjectId};

use rustix::{self, fd::OwnedFd};

use crate::{
    gaps::{Anchor, GapConfig},
    wayland,
};

const NAMESPACE: &str = "waygaps";

pub struct Surface {
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
}

impl Surface {
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
        wayland::wl_compositor::req::create_surface(backend, wl_compositor, wl_surface)?;

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
            Anchor::TopLeft => (wlrAnchor::LEFT | wlrAnchor::TOP, config.size, config.size),
            Anchor::TopRight => (wlrAnchor::RIGHT | wlrAnchor::TOP, config.size, config.size),
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

        wayland::zwlr_layer_surface_v1::req::set_anchor(backend, layer_surface, anchor)?;
        wayland::zwlr_layer_surface_v1::req::set_size(backend, layer_surface, width, height)?;

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
        })
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
