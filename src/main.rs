#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use ::tracing::trace;
use rustix::{self, fd::OwnedFd};
use waybackend::{objman, types::ObjectId};

use crate::{seat::Seat, surface::Surface, surface::WaylandObject};

mod config;
mod gaps;
mod seat;
mod surface;
mod tracing;
mod wayland;

#[global_allocator]
static GLOBAL_ALLOCATOR: talc::Talck<
    talc::locking::AssumeUnlockable,
    OomHandler,
> = talc::Talc::new(OomHandler).lock();

struct OomHandler;

impl talc::OomHandler for OomHandler {
    #[cold]
    #[inline(never)]
    fn handle_oom(
        talc: &mut talc::Talc<Self>,
        layout: core::alloc::Layout,
    ) -> Result<(), ()> {
        // We need at least ~1KB for talc's metadata. We allocate twice that to be sure.
        // Besides that, we round our allocation up to the next group of 1MB, so that we waste
        // less space to the metadata, and have to do less allocations overall
        let len = ((1 << 11) + layout.size()).next_multiple_of(1 << 20);

        // Note: as an optimization, we could use mremap on linux to extend
        // the allocation size "in_place", for a efficient realloc.
        // However, by not supporting mremap, we do not need to keep track
        // of the last allocations's ptr and size. Because we are allocating a
        // lot of data every time, this ends up winning in the end
        let ptr = unsafe {
            rustix::mm::mmap_anonymous(
                core::ptr::null_mut(),
                len,
                rustix::mm::ProtFlags::READ.union(rustix::mm::ProtFlags::WRITE),
                rustix::mm::MapFlags::PRIVATE,
            )
        };

        match ptr {
            Ok(ptr) => {
                unsafe {
                    talc.claim(talc::Span::from_base_size(ptr.cast(), len))?
                };
                trace!("new allocation of size: {len}");
                Ok(())
            }
            _ => Err(()),
        }
    }
}

#[cold]
#[inline(never)]
#[panic_handler]
#[cfg(not(test))]
fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    use ::tracing::error;
    if let Some(loc) = info.location() {
        error!(
            "PANIC AT {}:{}:{}: {}",
            loc.file(),
            loc.line(),
            loc.column(),
            info.message()
        );
    } else {
        error!("PANIC: {}", info.message());
    }
    origin::program::exit(-2);
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn origin_main(_: isize, _: *mut *mut u8, envp: *mut *mut u8) -> core::ffi::c_int {
    0
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
    fn new(
        backend: waybackend::Waybackend,
        objman: objman::ObjectManager<WaylandObject>,
    ) -> Self {
        let registry = objman.get_first(WaylandObject::Registry).unwrap();
        let compositor = objman.get_first(WaylandObject::Compositor).unwrap();
        let layer_shell = objman.get_first(WaylandObject::LayerShell).unwrap();
        let shm = objman.get_first(WaylandObject::Shm).unwrap();

        let (pipe_read, pipe_write) = rustix::pipe::pipe_with(
            rustix::pipe::PipeFlags::NONBLOCK
                .union(rustix::pipe::PipeFlags::CLOEXEC),
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
