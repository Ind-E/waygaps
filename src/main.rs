#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::{ffi::CStr, sync::atomic::AtomicBool};

use rustix::{self, fd::OwnedFd};
use tracing::{
    Level, debug, error, info, subscriber::set_global_default, trace,
};
use waybackend::{objman, types::ObjectId};

use crate::{
    log::LinuxSubscriber,
    seat::Seat,
    surface::{Surface, WaylandObject},
};

mod config;
mod gaps;
mod log;
mod seat;
mod surface;
mod utils;
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

#[unsafe(no_mangle)]
pub static mut environ: *const *const core::ffi::c_char = core::ptr::null();

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
pub extern "C" fn origin_main(
    _: isize,
    _: *mut *mut u8,
    envp: *mut *mut u8,
) -> core::ffi::c_int {
    unsafe { environ = envp.cast() };

    let subscriber = LinuxSubscriber::new(Level::TRACE);
    set_global_default(subscriber).unwrap();

    let (mut backend, mut objman, mut receiver) =
        utils::connect(WaylandObject::Display);
    let registry = objman.create(WaylandObject::Registry);
    let callback = objman.create(WaylandObject::Callback);

    let mut outputs = Vec::new();
    let mut seats = Vec::new();
    waybackend::roundtrip(
        &mut backend,
        &mut receiver,
        registry,
        callback,
        |backend, global| {
            use WaylandObject::*;
            use wayland::*;

            waybackend::bind_globals!(
                backend,
                objman,
                registry,
                global,
                |_, _, global: waybackend::Global| match global.interface() {
                    wayland::wl_output::NAME =>
                        outputs.push((global.name(), global.version())),
                    wayland::wl_seat::NAME =>
                        seats.push((global.name(), global.version())),
                    _ => (),
                },
                (wl_compositor, Compositor),
                (wl_shm, Shm),
                (zwlr_layer_shell_v1, LayerShell),
            );
        },
    )
    .unwrap();

    setup_signals();

    let mut app = App::new(backend, objman);
    // for (registry_name, version) in outputs {
    //     app.create_surfaces(registry_name, version);
    // }
    //
    // for (registry_name, version) in seats {
    //     app.create_seat(registry_name, version);
    // }

    info!("init");
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

static EXIT: AtomicBool = AtomicBool::new(false);
static PLAYBACK_DIRTY: AtomicBool = AtomicBool::new(true);
static CAPTURE_DIRTY: AtomicBool = AtomicBool::new(true);

fn set_playback_dirty() {
    PLAYBACK_DIRTY.store(true, core::sync::atomic::Ordering::Relaxed);
}

fn is_playback_dirty() -> bool {
    PLAYBACK_DIRTY.swap(false, core::sync::atomic::Ordering::Relaxed)
}

fn set_capture_dirty() {
    CAPTURE_DIRTY.store(true, core::sync::atomic::Ordering::Relaxed);
}

fn is_capture_dirty() -> bool {
    CAPTURE_DIRTY.swap(false, core::sync::atomic::Ordering::Relaxed)
}

fn set_exit() {
    EXIT.store(true, core::sync::atomic::Ordering::Relaxed);
}

fn should_exit() -> bool {
    EXIT.load(core::sync::atomic::Ordering::Relaxed)
}

extern "C" fn signal_handler(s: core::ffi::c_int) {
    if s == origin::signal::Signal::USR1.as_raw() {
        set_playback_dirty();
    } else if s == origin::signal::Signal::USR2.as_raw() {
        set_capture_dirty();
    } else {
        set_exit();
    }
}

fn setup_signals() {
    use origin::signal::{
        Sigaction, SigactionFlags, Signal, sig_ign, sigaction,
    };
    // C data structure, expected to be zeroed out.
    let mut action = Sigaction {
        sa_handler_kernel: Some(signal_handler),
        sa_flags: SigactionFlags::empty(),
        #[cfg(not(any(
            target_arch = "csky",
            target_arch = "loongarch64",
            target_arch = "mips",
            target_arch = "mips32r6",
            target_arch = "mips64",
            target_arch = "mips64r6",
            target_arch = "riscv32",
            target_arch = "riscv64"
        )))]
        sa_restorer: None,
        sa_mask: rustix::runtime::KernelSigSet::empty(),
    };

    for signal in [
        Signal::INT,
        Signal::QUIT,
        Signal::TERM,
        Signal::HUP,
        Signal::USR1,
        Signal::USR2,
    ] {
        if let Err(e) = unsafe { sigaction(signal, Some(action.clone())) } {
            error!("Failed to install signal handler: {e}");
        }
    }

    action.sa_handler_kernel = sig_ign();
    if let Err(e) = unsafe { sigaction(Signal::CHILD, Some(action)) } {
        error!("Failed to install signal handler: {e}");
    }

    debug!("Finished setting up signal handlers");
}

#[inline(never)]
fn shell_command(command: &CStr) {
    match unsafe { rustix::runtime::kernel_fork() } {
        Ok(rustix::runtime::Fork::Child(_)) => unsafe {
            let args: [*const u8; 5] = [
                c"env".as_ptr().cast(),
                c"bash".as_ptr().cast(),
                c"-c".as_ptr().cast(),
                command.as_ptr().cast(),
                core::ptr::null(),
            ];
            let err = rustix::runtime::execve(
                c"/usr/bin/env",
                args.as_ptr(),
                environ.cast(),
            );
            panic!("execve failed: {err}");
        },
        Err(e) => error!("fork failed: {e}"),
        _ => {}
    }
}
