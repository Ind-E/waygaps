use std::path::PathBuf;

use clap::Parser;
use waybackend::{objman, types::ObjectId};

use rustix::{self, fd::OwnedFd};

use crate::{seat::Seat, surface::Surface, surface::WaylandObject};

mod config;
mod gaps;
mod seat;
mod surface;
mod wayland;

#[derive(Parser)]
#[clap(version = env!("CARGO_PKG_VERSION"))]
struct Args {
    /// Config file path.
    #[clap(short, long, default_value = "~/.config/wayagps/config.kdl")]
    config: PathBuf,
    /// Preview the corners on your screen(s).
    #[clap(short, long)]
    preview: bool,
}

fn main() {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
}

struct App {
    backend: waybackend::Waybackend,
    objman: objman::ObjectManager<WaylandObject>,
    registry: ObjectId,
    compositor: ObjectId,
    shm: ObjectId,
    layer_shell: ObjectId,
    surfaces: Vec<Surface>,
    seats: Vec<Seat>,

    pipe_read: OwnedFd,
    pipe_write: OwnedFd,
}

impl App {
    fn new(backend: waybackend::Waybackend, objman: objman::ObjectManager<WaylandObject>) -> Self {
        let registry = objman.get_first(WaylandObject::Registry).unwrap();
        let compositor = objman.get_first(WaylandObject::Compositor).unwrap();
        let layer_shell = objman.get_first(WaylandObject::LayerShell).unwrap();
        let shm = objman.get_first(WaylandObject::Shm).unwrap();

        let (pipe_read, pipe_write) = rustix::pipe::pipe_with(
            rustix::pipe::PipeFlags::NONBLOCK.union(rustix::pipe::PipeFlags::CLOEXEC),
        )
        .unwrap();

        App {
            backend,
            objman,
            registry,
            compositor,
            shm,
            layer_shell,
            surfaces: Vec::with_capacity(1),
            seats: Vec::with_capacity(1),
            pipe_read,
            pipe_write,
        }
    }
}
