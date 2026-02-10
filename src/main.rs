#![no_std]
#![no_main]
#![feature(cstr_display)]

extern crate alloc;

use core::{
    ffi::{CStr, c_char, c_int},
    mem::MaybeUninit,
    sync::atomic::{self, AtomicBool},
};

use rustix::{
    self,
    event::epoll,
    process::{self},
};
use smallvec::SmallVec;
use waybackend::{
    Waybackend,
    objman::{self, ObjectManager},
    types::{ObjectId, WlFixed},
};

use crate::{
    config::{Anchor, GapConfig, InputEvent, read_config},
    gap::{WayGap, WaylandObject},
    seat::{Pointer, Seat},
    utils::{is_output_match, parse_args},
};

mod config;
mod gap;
mod log;
mod seat;
mod utils;
mod wayland;

const BUFFER_SCALE: u32 = 1;

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
        // We need at least ~1KB for talc's metadata. We allocate twice that to
        // be sure. Besides that, we round our allocation up to the next
        // group of 1MB, so that we waste less space to the metadata,
        // and have to do less allocations overall
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
                log::trace!("new allocation of size: {len}");
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
    if let Some(loc) = info.location() {
        log::fatal!(
            "PANIC AT {}:{}:{}: {}",
            loc.file(),
            loc.line(),
            loc.column(),
            info.message()
        );
    } else {
        log::fatal!("PANIC: {}", info.message());
    }
    origin::program::exit(-2);
}

#[unsafe(no_mangle)]
pub extern "C" fn origin_main(
    argc: c_int,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    unsafe { environ = envp.cast() };

    #[cfg(not(debug_assertions))]
    log::init(log::Filter::Info);
    #[cfg(debug_assertions)]
    log::init(log::Filter::Trace);

    let args = parse_args(argc, argv);

    // lower our process niceness priority. It's ok to delay updating the gaps
    // if the system is under heavy load
    let _ = rustix::process::nice(1);

    let configs = read_config(args.config_path);
    log::trace!("config: {configs:#?}");

    let (mut backend, mut objman, mut receiver) =
        utils::connect(WaylandObject::Display);
    let registry = objman.create(WaylandObject::Registry);
    let callback = objman.create(WaylandObject::Callback);

    let mut outputs = SmallVec::<[(u32, u32); 2]>::new();
    let mut seats = SmallVec::<[(u32, u32); 2]>::new();
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
                (zwp_relative_pointer_manager_v1, RelativePointerMgr),
            );
        },
    )
    .unwrap();

    setup_signals();

    let pid = process::getpid();
    log::info!("pid: {}", pid);

    let mut app = App::new(backend, objman, configs, args.preview);

    for (registry_name, version) in outputs {
        let wl_output = app.objman.create(WaylandObject::Output);

        app.pending_outputs.push(PendingOutput {
            registry_name,
            id: wl_output,
            name: <&str>::default(),
            description: <&str>::default(),
        });

        wayland::wl_registry::req::bind(
            &mut app.backend,
            app.registry,
            registry_name,
            wl_output,
            wayland::wl_output::NAME,
            version,
        )
        .unwrap();
    }

    for (registry_name, version) in seats {
        app.create_seat(registry_name, version);
    }

    let epoll_fd = epoll::create(epoll::CreateFlags::CLOEXEC).unwrap();
    const EPOLL_EV_WAYLAND: u64 = 0;

    epoll::add(
        &epoll_fd,
        &app.backend.wayland_fd,
        epoll::EventData::new_u64(EPOLL_EV_WAYLAND),
        epoll::EventFlags::IN.union(epoll::EventFlags::ERR),
    )
    .unwrap();

    let mut event_buffer = [MaybeUninit::uninit(); 32];

    while !should_exit() {
        use WaylandObject::*;
        use wayland::*;

        app.backend.flush().unwrap();

        let ready_events = match epoll::wait(&epoll_fd, &mut event_buffer, None)
        {
            Ok((ready_events, _unused_space)) => ready_events,
            Err(rustix::io::Errno::INTR | rustix::io::Errno::WOULDBLOCK) => {
                continue;
            }
            Err(e) => panic!("epoll failed: {e}"),
        };

        for event in ready_events {
            let flags = event.flags;
            if flags.contains(epoll::EventFlags::ERR) {
                log::error!("epoll event error on event: {}", event.data.u64());
                continue;
            }

            match event.data.u64() {
                EPOLL_EV_WAYLAND => {
                    let mut msgs =
                        receiver.recv(&app.backend.wayland_fd).unwrap();
                    while let Some(sender_id) = msgs.next() {
                        let sender_id = match sender_id {
                            Ok(sender_id) => sender_id,
                            Err(_) => {
                                log::warn!(
                                    "received a null object id from the server. This is a protocol violation!"
                                );
                                continue;
                            }
                        };
                        let sender = app.objman.get(sender_id).unwrap();
                        waybackend::match_enum_with_interface!(
                            app,
                            sender,
                            msgs,
                            (Display, wl_display),
                            (Registry, wl_registry),
                            (Callback, wl_callback),
                            (Compositor, wl_compositor),
                            (Seat, wl_seat),
                            (Shm, wl_shm),
                            (ShmPool, wl_shm_pool),
                            (Buffer, wl_buffer),
                            (Surface, wl_surface),
                            (Output, wl_output),
                            (Pointer, wl_pointer),
                            (RelativePointer, zwp_relative_pointer_v1),
                            (
                                RelativePointerMgr,
                                zwp_relative_pointer_manager_v1
                            ),
                            (LayerShell, zwlr_layer_shell_v1),
                            (LayerSurface, zwlr_layer_surface_v1),
                        );
                    }
                }
                otherwise => {
                    log::error!("epoll returned unexpected event: {otherwise}");
                }
            }
        }
    }

    0
}

struct PendingOutput {
    registry_name: u32,
    id: ObjectId,
    name: &'static str,
    description: &'static str,
}

pub type Configs = SmallVec<[(&'static str, GapConfig); 6]>;

struct App {
    backend: waybackend::Waybackend,
    objman: objman::ObjectManager<WaylandObject>,
    registry: ObjectId,
    compositor: ObjectId,
    shm: ObjectId,
    layer_shell: ObjectId,
    relative_ptr_mgr: ObjectId,
    waygaps: SmallVec<[WayGap; 6]>,
    seats: SmallVec<[Seat; 2]>,
    pending_outputs: SmallVec<[PendingOutput; 2]>,
    configs: Configs,

    preview: bool,
}

impl App {
    fn new(
        backend: waybackend::Waybackend,
        objman: objman::ObjectManager<WaylandObject>,
        configs: Configs,
        preview: bool,
    ) -> Self {
        let registry = objman.get_first(WaylandObject::Registry).unwrap();
        let compositor = objman.get_first(WaylandObject::Compositor).unwrap();
        let layer_shell = objman.get_first(WaylandObject::LayerShell).unwrap();
        let shm = objman.get_first(WaylandObject::Shm).unwrap();
        let relative_ptr_mgr =
            objman.get_first(WaylandObject::RelativePointerMgr).unwrap();

        App {
            backend,
            objman,
            registry,
            compositor,
            shm,
            layer_shell,
            relative_ptr_mgr,
            waygaps: SmallVec::with_capacity(1),
            seats: SmallVec::with_capacity(1),
            pending_outputs: SmallVec::new(),
            configs,
            preview,
        }
    }

    fn create_seat(&mut self, registry_name: u32, version: u32) {
        let wl_seat = self.objman.create(WaylandObject::Seat);
        wayland::wl_registry::req::bind(
            &mut self.backend,
            self.registry,
            registry_name,
            wl_seat,
            wayland::wl_seat::NAME,
            version,
        )
        .unwrap();

        self.seats.push(Seat::new(registry_name, wl_seat));
    }
}

// not a method to avoid borrowing shenanigans
fn create_waygap(
    backend: &mut Waybackend,
    objman: &mut ObjectManager<WaylandObject>,
    compositor: ObjectId,
    layer_shell: ObjectId,
    registry_name: u32,
    wl_output: ObjectId,
    config: &GapConfig,
    waygaps: &mut SmallVec<[WayGap; 6]>,
) {
    match WayGap::new(
        backend,
        objman,
        registry_name,
        compositor,
        layer_shell,
        wl_output,
        config,
    ) {
        Ok(waygap) => waygaps.push(waygap),
        Err(e) => log::error!("failed to create waygap: {e}"),
    }
}

impl wayland::wl_display::EvHandler for App {
    /// fatal error event
    ///
    /// The error event is sent out when a fatal (non-recoverable)
    /// error has occurred.  The object_id argument is the object
    /// where the error occurred, most often in response to a request
    /// to that object.  The code identifies the error and is defined
    /// by the object interface.  As such, each interface defines its
    /// own set of error codes.  The message is a brief description
    /// of the error, for (debugging) convenience.
    fn error(
        &mut self,
        _: ObjectId,
        object_id: ObjectId,
        code: u32,
        message: &str,
    ) {
        log::error!(
            "WAYLAND PROTOCOL ERROR: object: {object_id}, code: {code}, message:\n{message}\n"
        );
        set_exit();
    }

    /// acknowledge object ID deletion
    ///
    /// This event is used internally by the object ID management
    /// logic. When a client deletes an object that it had created,
    /// the server will send this event to acknowledge that it has
    /// seen the delete request. When the client receives this event,
    /// it will know that it can safely reuse the object ID.
    fn delete_id(&mut self, _: ObjectId, id: u32) {
        self.objman.remove(id);
    }
}

impl wayland::wl_registry::EvHandler for App {
    /// announce global object
    ///
    /// Notify the client of global objects.
    ///
    /// The event notifies the client that a global object with
    /// the given name is now available, and it implements the
    /// given version of the given interface.
    fn global(
        &mut self,
        _: ObjectId,
        name: u32,
        interface: &str,
        version: u32,
    ) {
        match interface {
            wayland::wl_seat::NAME => self.create_seat(name, version),
            _ => (),
        }
    }

    /// announce removal of global object
    ///
    /// Notify the client of removed global objects.
    ///
    /// This event notifies the client that the global identified
    /// by name is no longer available.  If the client bound to
    /// the global using the bind request, the client should now
    /// destroy that object.
    ///
    /// The object remains valid and requests to the object will be
    /// ignored until the client destroys it, to avoid races between
    /// the global going away and a client sending a request to it.
    fn global_remove(&mut self, _: ObjectId, name: u32) {
        if let Some(i) = self
            .waygaps
            .iter()
            .position(|waygap| waygap.registry_name == name)
        {
            self.waygaps.swap_remove(i);
        } else if let Some(i) = self
            .seats
            .iter()
            .position(|seat| seat.registry_name == name)
        {
            self.seats.swap_remove(i);
        }
    }
}

impl wayland::wl_callback::EvHandler for App {
    /// done event
    ///
    /// Notify the client when the related request is done.
    ///
    /// THIS IS A DESTRUCTOR
    fn done(&mut self, _sender_id: ObjectId, _: u32) {
        // NoOp (needed for rountrip)
    }
}

impl wayland::wl_compositor::EvHandler for App {}

impl wayland::wl_seat::EvHandler for App {
    /// seat capabilities changed
    ///
    /// This is sent on binding to the seat global or whenever a seat gains
    /// or loses the pointer, keyboard or touch capabilities.
    /// The argument is a capability enum containing the complete set of
    /// capabilities this seat has.
    ///
    /// When the pointer capability is added, a client may create a
    /// wl_pointer object using the wl_seat.get_pointer request. This object
    /// will receive pointer events until the capability is removed in the
    /// future.
    ///
    /// When the pointer capability is removed, a client should destroy the
    /// wl_pointer objects associated with the seat where the capability was
    /// removed, using the wl_pointer.release request. No further pointer
    /// events will be received on these objects.
    ///
    /// In some compositors, if a seat regains the pointer capability and a
    /// client has a previously obtained wl_pointer object of version 4 or
    /// less, that object may start sending pointer events again. This
    /// behavior is considered a misinterpretation of the intended behavior
    /// and must not be relied upon by the client. wl_pointer objects of
    /// version 5 or later must not send events if created before the most
    /// recent event notifying the client of an added pointer capability.
    ///
    /// The above behavior also applies to wl_keyboard and wl_touch with the
    /// keyboard and touch capabilities, respectively.
    fn capabilities(
        &mut self,
        seat: ObjectId,
        capabilities: wayland::wl_seat::Capability,
    ) {
        if let Some(seat) = self.seats.iter_mut().find(|s| s.wl_seat == seat) {
            if capabilities.contains(wayland::wl_seat::Capability::POINTER)
                && seat.pointer.is_none()
            {
                let ptr_id = self.objman.create(WaylandObject::Pointer);
                wayland::wl_seat::req::get_pointer(
                    &mut self.backend,
                    seat.wl_seat,
                    ptr_id,
                )
                .unwrap();

                let mut pointer = Pointer::new(ptr_id);

                let relative_ptr_id =
                    self.objman.create(WaylandObject::RelativePointer);
                wayland::zwp_relative_pointer_manager_v1::req::get_relative_pointer(
                    &mut self.backend,
                    self.relative_ptr_mgr,
                    relative_ptr_id,
                    ptr_id,
                ).unwrap();

                pointer.relative_pointer_id = Some(relative_ptr_id);

                seat.pointer = Some(pointer);
            } else if !capabilities
                .contains(wayland::wl_seat::Capability::POINTER)
                && let Some(ptr) = seat.pointer.take()
            {
                if let Some(rel_ptr_id) = ptr.relative_pointer_id {
                    wayland::zwp_relative_pointer_v1::req::destroy(
                        &mut self.backend,
                        rel_ptr_id,
                    )
                    .unwrap();
                }

                // we no longer have a pointer, release the previous object
                wayland::wl_pointer::req::release(
                    &mut self.backend,
                    seat.wl_seat,
                )
                .unwrap();
            }
        }
    }

    /// unique identifier for this seat
    ///
    /// In a multi-seat configuration the seat name can be used by clients to
    /// help identify which physical devices the seat represents.
    ///
    /// The seat name is a UTF-8 string with no convention defined for its
    /// contents. Each name is unique among all wl_seat globals. The name is
    /// only guaranteed to be unique for the current compositor instance.
    ///
    /// The same seat names are used for all clients. Thus, the name can be
    /// shared across processes to refer to a specific wl_seat global.
    ///
    /// The name event is sent after binding to the seat global, and should be
    /// sent before announcing capabilities. This event only sent once per
    /// seat object, and the name does not change over the lifetime of the
    /// wl_seat global.
    ///
    /// Compositors may re-use the same seat name if the wl_seat global is
    /// destroyed and re-created later.
    fn name(&mut self, _: ObjectId, _: &str) {
        // NoOp
    }
}

impl wayland::wl_shm::EvHandler for App {
    /// pixel format description
    ///
    /// Informs the client about a valid pixel format that
    /// can be used for buffers. Known formats include
    /// argb8888 and xrgb8888.
    fn format(&mut self, _: ObjectId, _: wayland::wl_shm::Format) {
        // ignore all messages since we will simply use ARGB8888 every time
    }
}

impl wayland::wl_shm_pool::EvHandler for App {}

impl wayland::wl_buffer::EvHandler for App {
    /// compositor releases buffer
    ///
    /// Sent when this wl_buffer is no longer used by the compositor.
    ///
    /// For more information on when release events may or may not be sent,
    /// and what consequences it has, please see the description of
    /// wl_surface.attach.
    ///
    /// If a client receives a release event before the frame callback
    /// requested in the same wl_surface.commit that attaches this
    /// wl_buffer to a surface, then the client is immediately free to
    /// reuse the buffer and its backing storage, and does not need a
    /// second buffer for the next surface content update. Typically
    /// this is possible, when the compositor maintains a copy of the
    /// wl_surface contents, e.g. as a GL texture. This is an important
    /// optimization for GL(ES) compositors with wl_shm clients.
    fn release(&mut self, sender_id: ObjectId) {
        wayland::wl_buffer::req::destroy(&mut self.backend, sender_id).unwrap();
    }
}

impl wayland::wl_surface::EvHandler for App {
    /// surface enters an output
    ///
    /// This is emitted whenever a surface's creation, movement, or resizing
    /// results in some part of it being within the scanout region of an
    /// output.
    ///
    /// Note that a surface may be overlapping with zero or more outputs.
    fn enter(&mut self, _sender_id: ObjectId, _output: ObjectId) {
        // NoOp
    }

    /// surface leaves an output
    ///
    /// This is emitted whenever a surface's creation, movement, or resizing
    /// results in it no longer having any part of it within the scanout region
    /// of an output.
    ///
    /// Clients should not use the number of outputs the surface is on for frame
    /// throttling purposes. The surface might be hidden even if no leave event
    /// has been sent, and the compositor might expect new surface content
    /// updates even if no enter event has been sent. The frame event should be
    /// used instead.
    fn leave(&mut self, _sender_id: ObjectId, _output: ObjectId) {
        // NoOp
    }

    /// preferred buffer scale for the surface
    ///
    /// This event indicates the preferred buffer scale for this surface. It is
    /// sent whenever the compositor's preference changes.
    ///
    /// Before receiving this event the preferred buffer scale for this surface
    /// is 1.
    ///
    /// It is intended that scaling aware clients use this event to scale their
    /// content and use wl_surface.set_buffer_scale to indicate the scale they
    /// have rendered with. This allows clients to supply a higher detail
    /// buffer.
    ///
    /// The compositor shall emit a scale value greater than 0.
    fn preferred_buffer_scale(&mut self, _sender_id: ObjectId, _factor: i32) {
        // NoOp
    }

    /// preferred buffer transform for the surface
    ///
    /// This event indicates the preferred buffer transform for this surface.
    /// It is sent whenever the compositor's preference changes.
    ///
    /// Before receiving this event the preferred buffer transform for this
    /// surface is normal.
    ///
    /// Applying this transformation to the surface buffer contents and using
    /// wl_surface.set_buffer_transform might allow the compositor to use the
    /// surface buffer more efficiently.
    fn preferred_buffer_transform(
        &mut self,
        _sender_id: ObjectId,
        _transform: wayland::wl_output::Transform,
    ) {
        // NoOp
    }
}

impl wayland::wl_output::EvHandler for App {
    /// properties of the output
    ///
    /// The geometry event describes geometric properties of the output.
    /// The event is sent when binding to the output object and whenever
    /// any of the properties change.
    ///
    /// The physical size can be set to zero if it doesn't make sense for this
    /// output (e.g. for projectors or virtual outputs).
    ///
    /// The geometry event will be followed by a done event (starting from
    /// version 2).
    ///
    /// Clients should use wl_surface.preferred_buffer_transform instead of the
    /// transform advertised by this event to find the preferred buffer
    /// transform to use for a surface.
    ///
    /// Note: wl_output only advertises partial information about the output
    /// position and identification. Some compositors, for instance those not
    /// implementing a desktop-style output layout or those exposing virtual
    /// outputs, might fake this information. Instead of using x and y, clients
    /// should use xdg_output.logical_position. Instead of using make and model,
    /// clients should use name and description.
    fn geometry(
        &mut self,
        _sender_id: ObjectId,
        _x: i32,
        _y: i32,
        _physical_width: i32,
        _physical_height: i32,
        _subpixel: wayland::wl_output::Subpixel,
        _make: &str,
        _model: &str,
        _transform: wayland::wl_output::Transform,
    ) {
        // NoOp
    }

    /// advertise available modes for the output
    ///
    /// The mode event describes an available mode for the output.
    ///
    /// The event is sent when binding to the output object and there
    /// will always be one mode, the current mode.  The event is sent
    /// again if an output changes mode, for the mode that is now
    /// current.  In other words, the current mode is always the last
    /// mode that was received with the current flag set.
    ///
    /// Non-current modes are deprecated. A compositor can decide to only
    /// advertise the current mode and never send other modes. Clients
    /// should not rely on non-current modes.
    ///
    /// The size of a mode is given in physical hardware units of
    /// the output device. This is not necessarily the same as
    /// the output size in the global compositor space. For instance,
    /// the output may be scaled, as described in wl_output.scale,
    /// or transformed, as described in wl_output.transform. Clients
    /// willing to retrieve the output size in the global compositor
    /// space should use xdg_output.logical_size instead.
    ///
    /// The vertical refresh rate can be set to zero if it doesn't make
    /// sense for this output (e.g. for virtual outputs).
    ///
    /// The mode event will be followed by a done event (starting from
    /// version 2).
    ///
    /// Clients should not use the refresh rate to schedule frames. Instead,
    /// they should use the wl_surface.frame event or the presentation-time
    /// protocol.
    ///
    /// Note: this information is not always meaningful for all outputs. Some
    /// compositors, such as those exposing virtual outputs, might fake the
    /// refresh rate or the size.
    fn mode(
        &mut self,
        _sender_id: ObjectId,
        _flags: wayland::wl_output::Mode,
        _width: i32,
        _height: i32,
        _refresh: i32,
    ) {
        // NoOp
    }

    /// sent all information about output
    ///
    /// This event is sent after all other properties have been
    /// sent after binding to the output object and after any
    /// other property changes done after that. This allows
    /// changes to the output properties to be seen as
    /// atomic, even if they happen via multiple events.
    fn done(&mut self, sender_id: ObjectId) {
        if self.waygaps.iter().any(|gap| gap.wl_output == sender_id) {
            return;
        }

        let Some(index) =
            self.pending_outputs.iter().position(|o| o.id == sender_id)
        else {
            return;
        };

        let output = self.pending_outputs.swap_remove(index);

        for (cfg_name, cfg) in &self.configs {
            if is_output_match(cfg.output, output.name, output.description) {
                log::info!(
                    "opening config `{cfg_name}` on output `{}`",
                    output.name
                );
                create_waygap(
                    &mut self.backend,
                    &mut self.objman,
                    self.compositor,
                    self.layer_shell,
                    output.registry_name,
                    sender_id,
                    cfg,
                    &mut self.waygaps,
                );
            }
        }
    }

    /// output scaling properties
    ///
    /// This event contains scaling geometry information
    /// that is not in the geometry event. It may be sent after
    /// binding the output object or if the output scale changes
    /// later. The compositor will emit a non-zero, positive
    /// value for scale. If it is not sent, the client should
    /// assume a scale of 1.
    ///
    /// A scale larger than 1 means that the compositor will
    /// automatically scale surface buffers by this amount
    /// when rendering. This is used for very high resolution
    /// displays where applications rendering at the native
    /// resolution would be too small to be legible.
    ///
    /// Clients should use wl_surface.preferred_buffer_scale
    /// instead of this event to find the preferred buffer
    /// scale to use for a surface.
    ///
    /// The scale event will be followed by a done event.
    fn scale(&mut self, _sender_id: ObjectId, _factor: i32) {
        // NoOp
    }

    /// name of this output
    ///
    /// Many compositors will assign user-friendly names to their outputs, show
    /// them to the user, allow the user to refer to an output, etc. The client
    /// may wish to know this name as well to offer the user similar behaviors.
    ///
    /// The name is a UTF-8 string with no convention defined for its contents.
    /// Each name is unique among all wl_output globals. The name is only
    /// guaranteed to be unique for the compositor instance.
    ///
    /// The same output name is used for all clients for a given wl_output
    /// global. Thus, the name can be shared across processes to refer to a
    /// specific wl_output global.
    ///
    /// The name is not guaranteed to be persistent across sessions, thus cannot
    /// be used to reliably identify an output in e.g. configuration files.
    ///
    /// Examples of names include 'HDMI-A-1', 'WL-1', 'X11-1', etc. However, do
    /// not assume that the name is a reflection of an underlying DRM connector,
    /// X11 connection, etc.
    ///
    /// The name event is sent after binding the output object. This event is
    /// only sent once per output object, and the name does not change over the
    /// lifetime of the wl_output global.
    ///
    /// Compositors may re-use the same output name if the wl_output global is
    /// destroyed and re-created later. Compositors should avoid re-using the
    /// same name if possible.
    ///
    /// The name event will be followed by a done event.
    fn name(&mut self, sender_id: ObjectId, name: &str) {
        if let Some(out) =
            self.pending_outputs.iter_mut().find(|o| o.id == sender_id)
        {
            let static_name = unsafe { core::mem::transmute(name) };
            out.name = static_name;
        }
    }

    /// human-readable description of this output
    ///
    /// Many compositors can produce human-readable descriptions of their
    /// outputs. The client may wish to know this description as well, e.g. for
    /// output selection purposes.
    ///
    /// The description is a UTF-8 string with no convention defined for its
    /// contents. The description is not guaranteed to be unique among all
    /// wl_output globals. Examples might include 'Foocorp 11\" Display' or
    /// 'Virtual X11 output via :1'.
    ///
    /// The description event is sent after binding the output object and
    /// whenever the description changes. The description is optional, and may
    /// not be sent at all.
    ///
    /// The description event will be followed by a done event.
    fn description(&mut self, sender_id: ObjectId, description: &str) {
        if let Some(out) =
            self.pending_outputs.iter_mut().find(|o| o.id == sender_id)
        {
            let static_description =
                unsafe { core::mem::transmute(description) };
            out.description = static_description;
        }
    }
}

impl wayland::wl_pointer::EvHandler for App {
    /// enter event
    ///
    /// Notification that this seat's pointer is focused on a certain
    /// surface.
    ///
    /// When a seat's focus enters a surface, the pointer image
    /// is undefined and a client should respond to this event by setting
    /// an appropriate pointer image with the set_cursor request.
    fn enter(
        &mut self,
        sender_id: ObjectId,
        serial: u32,
        surface: ObjectId,
        _surface_x: WlFixed,
        _surface_y: WlFixed,
    ) {
        let current_waygap = 'brk: {
            for (i, gap) in self.waygaps.iter().enumerate() {
                if gap.wl_surface == surface {
                    break 'brk i as u32;
                }
            }
            return;
        };

        match get_pointer(&mut self.seats, sender_id) {
            Some(ptr) => {
                ptr.enter_serial = serial;
                ptr.current_waygap = current_waygap;

                if let Some(waygap) =
                    self.waygaps.get(ptr.current_waygap as usize)
                {
                    for event in waygap.commands.iter() {
                        if let (InputEvent::Enter, cmd) = event {
                            shell_command(cmd);
                            break;
                        }
                    }
                }

                ptr
            }
            None => return,
        };
    }

    /// leave event
    ///
    /// Notification that this seat's pointer is no longer focused on
    /// a certain surface.
    ///
    /// The leave notification is sent before the enter notification
    /// for the new focus.
    fn leave(&mut self, sender_id: ObjectId, _serial: u32, _surface: ObjectId) {
        if let Some(ptr) = get_pointer(&mut self.seats, sender_id) {
            if let Some(waygap) = self.waygaps.get(ptr.current_waygap as usize)
            {
                for event in waygap.commands.iter() {
                    if let (InputEvent::Leave, cmd) = event {
                        shell_command(cmd);
                        break;
                    }
                }
            }
            ptr.current_waygap = u32::MAX;
        }
    }

    /// pointer motion event
    ///
    /// Notification of pointer location change. The arguments
    /// surface_x and surface_y are the location relative to the
    /// focused surface.
    fn motion(
        &mut self,
        _sender_id: ObjectId,
        _time: u32,
        _surface_x: WlFixed,
        _surface_y: WlFixed,
    ) {
        // NoOp
    }

    /// pointer button event
    ///
    /// Mouse button click and release notifications.
    ///
    /// The location of the click is given by the last motion or
    /// enter event.
    /// The time argument is a timestamp with millisecond
    /// granularity, with an undefined base.
    ///
    /// The button is a button code as defined in the Linux kernel's
    /// linux/input-event-codes.h header file, e.g. BTN_LEFT.
    ///
    /// Any 16-bit button code value is reserved for future additions to the
    /// kernel's event code list. All other button codes above 0xFFFF are
    /// currently undefined but may be used in future versions of this
    /// protocol.
    fn button(
        &mut self,
        sender_id: ObjectId,
        _serial: u32,
        _time: u32,
        button: u32,
        state: wayland::wl_pointer::ButtonState,
    ) {
        if let Some(ptr) = get_pointer(&mut self.seats, sender_id) {
            ptr.button = match state {
                wayland::wl_pointer::ButtonState::released => 0,
                wayland::wl_pointer::ButtonState::pressed => button as u16,
            }
        }
    }

    /// axis event
    ///
    /// Scroll and other axis notifications.
    ///
    /// For scroll events (vertical and horizontal scroll axes), the
    /// value parameter is the length of a vector along the specified
    /// axis in a coordinate space identical to those of motion events,
    /// representing a relative movement along the specified axis.
    ///
    /// For devices that support movements non-parallel to axes multiple
    /// axis events will be emitted.
    ///
    /// When applicable, for example for touch pads, the server can
    /// choose to emit scroll events where the motion vector is
    /// equivalent to a motion event vector.
    ///
    /// When applicable, a client can transform its content relative to the
    /// scroll distance.
    fn axis(
        &mut self,
        _sender_id: ObjectId,
        _time: u32,
        _axis: wayland::wl_pointer::Axis,
        _value: WlFixed,
    ) {
        // NoOp
    }

    /// end of a pointer event sequence
    ///
    /// Indicates the end of a set of events that logically belong together.
    /// A client is expected to accumulate the data in all events within the
    /// frame before proceeding.
    ///
    /// All wl_pointer events before a wl_pointer.frame event belong
    /// logically together. For example, in a diagonal scroll motion the
    /// compositor will send an optional wl_pointer.axis_source event, two
    /// wl_pointer.axis events (horizontal and vertical) and finally a
    /// wl_pointer.frame event. The client may use this information to
    /// calculate a diagonal vector for scrolling.
    ///
    /// When multiple wl_pointer.axis events occur within the same frame,
    /// the motion vector is the combined motion of all events.
    /// When a wl_pointer.axis and a wl_pointer.axis_stop event occur within
    /// the same frame, this indicates that axis movement in one axis has
    /// stopped but continues in the other axis.
    /// When multiple wl_pointer.axis_stop events occur within the same
    /// frame, this indicates that these axes stopped in the same instance.
    ///
    /// A wl_pointer.frame event is sent for every logical event group,
    /// even if the group only contains a single wl_pointer event.
    /// Specifically, a client may get a sequence: motion, frame, button,
    /// frame, axis, frame, axis_stop, frame.
    ///
    /// The wl_pointer.enter and wl_pointer.leave events are logical events
    /// generated by the compositor and not the hardware. These events are
    /// also grouped by a wl_pointer.frame. When a pointer moves from one
    /// surface to another, a compositor should group the
    /// wl_pointer.leave event within the same wl_pointer.frame.
    /// However, a client must not rely on wl_pointer.leave and
    /// wl_pointer.enter being in the same wl_pointer.frame.
    /// Compositor-specific policies may require the wl_pointer.leave and
    /// wl_pointer.enter event being split across multiple wl_pointer.frame
    /// groups.
    fn frame(&mut self, sender_id: ObjectId) {
        let (ptr, waygap) = match get_pointer(&mut self.seats, sender_id) {
            Some(ptr) => match self.waygaps.get(ptr.current_waygap as usize) {
                Some(waygap) => (ptr, waygap),
                None => return,
            },
            None => return,
        };

        for event in waygap.commands.iter() {
            if let (InputEvent::Button(btn), cmd) = event
                && *btn == ptr.button
            {
                shell_command(cmd);
            }
        }
        ptr.button = 0;

        if ptr.value120 <= -120 {
            ptr.value120 += 120;
            for event in waygap.commands.iter() {
                if let (InputEvent::Scroll(scroll), cmd) = event
                    && scroll.on_axis(ptr.axis)
                    && !scroll.is_positive()
                {
                    shell_command(cmd);
                }
            }
        } else if ptr.value120 >= 120 {
            ptr.value120 -= 120;
            for event in waygap.commands.iter() {
                if let (InputEvent::Scroll(scroll), cmd) = event
                    && scroll.on_axis(ptr.axis)
                    && scroll.is_positive()
                {
                    shell_command(cmd);
                }
            }
        }

        if ptr.should_trigger_edge {
            for event in waygap.commands.iter() {
                if let (InputEvent::Edge, cmd) = event {
                    shell_command(cmd);
                    break;
                }
            }
            ptr.should_trigger_edge = false;
        }
    }

    /// axis source event
    ///
    /// Source information for scroll and other axes.
    ///
    /// This event does not occur on its own. It is sent before a
    /// wl_pointer.frame event and carries the source information for
    /// all events within that frame.
    ///
    /// The source specifies how this event was generated. If the source is
    /// wl_pointer.axis_source.finger, a wl_pointer.axis_stop event will be
    /// sent when the user lifts the finger off the device.
    ///
    /// If the source is wl_pointer.axis_source.wheel,
    /// wl_pointer.axis_source.wheel_tilt or
    /// wl_pointer.axis_source.continuous, a wl_pointer.axis_stop event may
    /// or may not be sent. Whether a compositor sends an axis_stop event
    /// for these sources is hardware-specific and implementation-dependent;
    /// clients must not rely on receiving an axis_stop event for these
    /// scroll sources and should treat scroll sequences from these scroll
    /// sources as unterminated by default.
    ///
    /// This event is optional. If the source is unknown for a particular
    /// axis event sequence, no event is sent.
    /// Only one wl_pointer.axis_source event is permitted per frame.
    ///
    /// The order of wl_pointer.axis_discrete and wl_pointer.axis_source is
    /// not guaranteed.
    fn axis_source(
        &mut self,
        _sender_id: ObjectId,
        _axis_source: wayland::wl_pointer::AxisSource,
    ) {
        // NoOp
    }

    /// axis stop event
    ///
    /// Stop notification for scroll and other axes.
    ///
    /// For some wl_pointer.axis_source types, a wl_pointer.axis_stop event
    /// is sent to notify a client that the axis sequence has terminated.
    /// This enables the client to implement kinetic scrolling.
    /// See the wl_pointer.axis_source documentation for information on when
    /// this event may be generated.
    ///
    /// Any wl_pointer.axis events with the same axis_source after this
    /// event should be considered as the start of a new axis motion.
    ///
    /// The timestamp is to be interpreted identical to the timestamp in the
    /// wl_pointer.axis event. The timestamp value may be the same as a
    /// preceding wl_pointer.axis event.
    fn axis_stop(
        &mut self,
        _sender_id: ObjectId,
        _time: u32,
        _axis: wayland::wl_pointer::Axis,
    ) {
        // NoOp
    }

    /// axis click event
    ///
    /// Discrete step information for scroll and other axes.
    ///
    /// This event carries the axis value of the wl_pointer.axis event in
    /// discrete steps (e.g. mouse wheel clicks).
    ///
    /// This event is deprecated with wl_pointer version 8 - this event is not
    /// sent to clients supporting version 8 or later.
    ///
    /// This event does not occur on its own, it is coupled with a
    /// wl_pointer.axis event that represents this axis value on a
    /// continuous scale. The protocol guarantees that each axis_discrete
    /// event is always followed by exactly one axis event with the same
    /// axis number within the same wl_pointer.frame. Note that the protocol
    /// allows for other events to occur between the axis_discrete and
    /// its coupled axis event, including other axis_discrete or axis
    /// events. A wl_pointer.frame must not contain more than one axis_discrete
    /// event per axis type.
    ///
    /// This event is optional; continuous scrolling devices
    /// like two-finger scrolling on touchpads do not have discrete
    /// steps and do not generate this event.
    ///
    /// The discrete value carries the directional information. e.g. a value
    /// of -2 is two steps towards the negative direction of this axis.
    ///
    /// The axis number is identical to the axis number in the associated
    /// axis event.
    ///
    /// The order of wl_pointer.axis_discrete and wl_pointer.axis_source is
    /// not guaranteed.
    fn axis_discrete(
        &mut self,
        sender_id: ObjectId,
        axis: wayland::wl_pointer::Axis,
        discrete: i32,
    ) {
        let ptr = match get_pointer(&mut self.seats, sender_id) {
            Some(ptr) => match self.waygaps.get(ptr.current_waygap as usize) {
                Some(_waygap) => ptr,
                None => return,
            },
            None => return,
        };

        ptr.axis = axis.into();
        match discrete {
            ..0 => ptr.value120 = -120,
            1.. => ptr.value120 = 120,
            0 => {}
        }
    }

    /// axis high-resolution scroll event
    ///
    /// Discrete high-resolution scroll information.
    ///
    /// This event carries high-resolution wheel scroll information,
    /// with each multiple of 120 representing one logical scroll step
    /// (a wheel detent). For example, an axis_value120 of 30 is one quarter of
    /// a logical scroll step in the positive direction, a value120 of
    /// -240 are two logical scroll steps in the negative direction within the
    /// same hardware event.
    /// Clients that rely on discrete scrolling should accumulate the
    /// value120 to multiples of 120 before processing the event.
    ///
    /// The value120 must not be zero.
    ///
    /// This event replaces the wl_pointer.axis_discrete event in clients
    /// supporting wl_pointer version 8 or later.
    ///
    /// Where a wl_pointer.axis_source event occurs in the same
    /// wl_pointer.frame, the axis source applies to this event.
    ///
    /// The order of wl_pointer.axis_value120 and wl_pointer.axis_source is
    /// not guaranteed.
    fn axis_value120(
        &mut self,
        sender_id: ObjectId,
        axis: wayland::wl_pointer::Axis,
        value120: i32,
    ) {
        let ptr = match get_pointer(&mut self.seats, sender_id) {
            Some(ptr) => match self.waygaps.get(ptr.current_waygap as usize) {
                Some(_waygap) => ptr,
                None => return,
            },
            None => return,
        };

        ptr.value120 += value120;
        ptr.axis = axis.into();
    }

    /// axis relative physical direction event
    ///
    /// Relative directional information of the entity causing the axis
    /// motion.
    ///
    /// For a wl_pointer.axis event, the wl_pointer.axis_relative_direction
    /// event specifies the movement direction of the entity causing the
    /// wl_pointer.axis event. For example:
    /// - if a user's fingers on a touchpad move down and this
    /// causes a wl_pointer.axis vertical_scroll down event, the physical
    /// direction is 'identical'
    /// - if a user's fingers on a touchpad move down and this causes a
    /// wl_pointer.axis vertical_scroll up scroll up event ('natural
    /// scrolling'), the physical direction is 'inverted'.
    ///
    /// A client may use this information to adjust scroll motion of
    /// components. Specifically, enabling natural scrolling causes the
    /// content to change direction compared to traditional scrolling.
    /// Some widgets like volume control sliders should usually match the
    /// physical direction regardless of whether natural scrolling is
    /// active. This event enables clients to match the scroll direction of
    /// a widget to the physical direction.
    ///
    /// This event does not occur on its own, it is coupled with a
    /// wl_pointer.axis event that represents this axis value.
    /// The protocol guarantees that each axis_relative_direction event is
    /// always followed by exactly one axis event with the same
    /// axis number within the same wl_pointer.frame. Note that the protocol
    /// allows for other events to occur between the axis_relative_direction
    /// and its coupled axis event.
    ///
    /// The axis number is identical to the axis number in the associated
    /// axis event.
    ///
    /// The order of wl_pointer.axis_relative_direction,
    /// wl_pointer.axis_discrete and wl_pointer.axis_source is not
    /// guaranteed.
    fn axis_relative_direction(
        &mut self,
        _sender_id: ObjectId,
        _axis: wayland::wl_pointer::Axis,
        _direction: wayland::wl_pointer::AxisRelativeDirection,
    ) {
        // NoOp
    }
}

impl wayland::zwp_relative_pointer_v1::EvHandler for App {
    /// relative pointer motion
    ///
    /// Relative x/y pointer motion from the pointer of the seat associated with
    /// this object.
    ///
    /// A relative motion is in the same dimension as regular wl_pointer motion
    /// events, except they do not represent an absolute position. For example,
    /// moving a pointer from (x, y) to (x', y') would have the equivalent
    /// relative motion (x' - x, y' - y). If a pointer motion caused the
    /// absolute pointer position to be clipped by for example the edge of the
    /// monitor, the relative motion is unaffected by the clipping and will
    /// represent the unclipped motion.
    ///
    /// This event also contains non-accelerated motion deltas. The
    /// non-accelerated delta is, when applicable, the regular pointer motion
    /// delta as it was before having applied motion acceleration and other
    /// transformations such as normalization.
    ///
    /// Note that the non-accelerated delta does not represent 'raw' events as
    /// they were read from some device. Pointer motion acceleration is device-
    /// and configuration-specific and non-accelerated deltas and accelerated
    /// deltas may have the same value on some devices.
    ///
    /// Relative motions are not coupled to wl_pointer.motion events, and can be
    /// sent in combination with such events, but also independently. There may
    /// also be scenarios where wl_pointer.motion is sent, but there is no
    /// relative motion. The order of an absolute and relative motion event
    /// originating from the same physical motion is not guaranteed.
    ///
    /// If the client needs button events or focus state, it can receive them
    /// from a wl_pointer object of the same seat that the wp_relative_pointer
    /// object is associated with.
    fn relative_motion(
        &mut self,
        sender_id: ObjectId,
        utime_hi: u32,
        utime_lo: u32,
        _dx: WlFixed,
        _dy: WlFixed,
        dx_unaccel: WlFixed,
        dy_unaccel: WlFixed,
    ) {
        let ptr = match get_relative_pointer(&mut self.seats, sender_id) {
            Some(ptr) => ptr,
            None => return,
        };

        let waygap = match self.waygaps.get(ptr.current_waygap as usize) {
            Some(waygap) => waygap,
            None => return,
        };

        let time: u64 = ((utime_hi as u64) << 32) | (utime_lo as u64);
        let dt = time.saturating_sub(ptr.last_time);

        if dt > 100 {
            ptr.pressure_x = 0.0;
            ptr.pressure_y = 0.0;
        }

        let dx = f64::from(dx_unaccel);
        let dy = f64::from(dy_unaccel);

        if matches!(
            waygap.anchor,
            Anchor::Left | Anchor::TopLeft | Anchor::BottomLeft
        ) {
            if dx < 0.0 {
                ptr.pressure_x += dx.abs();
            } else if dx > 1.0 {
                ptr.pressure_x = 0.0;
            }
        } else if matches!(
            waygap.anchor,
            Anchor::Right | Anchor::TopRight | Anchor::BottomRight
        ) {
            if dx > 0.0 {
                ptr.pressure_x += dx.abs();
            } else if dx < -1.0 {
                ptr.pressure_x = 0.0;
            }
        }

        if matches!(
            waygap.anchor,
            Anchor::Top | Anchor::TopLeft | Anchor::TopRight
        ) {
            if dy < 0.0 {
                ptr.pressure_y += dy.abs();
            } else if dy > 1.0 {
                ptr.pressure_y = 0.0;
            }
        } else if matches!(
            waygap.anchor,
            Anchor::Bottom | Anchor::BottomLeft | Anchor::BottomRight
        ) {
            if dy > 0.0 {
                ptr.pressure_y += dy.abs();
            } else if dy < -1.0 {
                ptr.pressure_y = 0.0;
            }
        }

        if ptr.pressure_x > waygap.activation_force as f64
            || ptr.pressure_y > waygap.activation_force as f64
        {
            if time - ptr.last_trigger_time > 200000 {
                ptr.should_trigger_edge = true;
                ptr.last_trigger_time = time;
            }
            ptr.pressure_x = 0.0;
            ptr.pressure_y = 0.0;
        }

        ptr.last_time = time;
    }
}

impl wayland::zwp_relative_pointer_manager_v1::EvHandler for App {}

fn get_pointer(seats: &mut [Seat], ptr_id: ObjectId) -> Option<&mut Pointer> {
    for seat in seats {
        if let Some(ptr) = seat.pointer.as_mut()
            && ptr.id == ptr_id
        {
            return Some(ptr);
        }
    }
    None
}

fn get_relative_pointer(
    seats: &mut [Seat],
    relative_ptr_id: ObjectId,
) -> Option<&mut Pointer> {
    for seat in seats {
        if let Some(ptr) = seat.pointer.as_mut()
            && ptr.relative_pointer_id == Some(relative_ptr_id)
        {
            return Some(ptr);
        }
    }
    None
}

impl wayland::zwlr_layer_shell_v1::EvHandler for App {}
impl wayland::zwlr_layer_surface_v1::EvHandler for App {
    /// suggest a surface change
    ///
    /// The configure event asks the client to resize its surface.
    ///
    /// Clients should arrange their surface for the new states, and then send
    /// an ack_configure request with the serial sent in this configure event at
    /// some point before committing the new surface.
    ///
    /// The client is free to dismiss all but the last configure event it
    /// received.
    ///
    /// The width and height arguments specify the size of the window in
    /// surface-local coordinates.
    ///
    /// The size is a hint, in the sense that the client is free to ignore it if
    /// it doesn't resize, pick a smaller size (to satisfy aspect ratio or
    /// resize in steps of NxM pixels). If the client picks a smaller size and
    /// is anchored to two opposite anchors (e.g. 'top' and 'bottom'), the
    /// surface will be centered on this axis.
    ///
    /// If the width or height arguments are zero, it means the client should
    /// decide its own window dimension.
    fn configure(
        &mut self,
        sender_id: ObjectId,
        serial: u32,
        width: u32,
        height: u32,
    ) {
        let width = width * BUFFER_SCALE;
        let height = height * BUFFER_SCALE;

        wayland::zwlr_layer_surface_v1::req::ack_configure(
            &mut self.backend,
            sender_id,
            serial,
        )
        .unwrap();

        if let Some(waygap) = self
            .waygaps
            .iter_mut()
            .find(|waygap| waygap.layer_surface == sender_id)
        {
            if waygap.configured
                && waygap.width == width
                && waygap.height == height
            {
                return;
            }

            waygap.width = width;
            waygap.height = height;
            waygap.expand_shm(waygap.width * waygap.height);

            waygap.configured = true;

            if self.preview {
                waygap.draw_preview();
            }

            if let Err(e) =
                waygap.draw_frame(&mut self.backend, self.shm, &mut self.objman)
            {
                log::error!("failed to draw frame: {e}")
            }
        }
    }

    /// surface should be closed
    ///
    /// The closed event is sent by the compositor when the surface will no
    /// longer be shown. The output may have been destroyed or the user may
    /// have asked for it to be removed. Further changes to the surface will be
    /// ignored. The client should destroy the resource after receiving this
    /// event, and create a new surface if they so choose.
    fn closed(&mut self, _sender_id: ObjectId) {
        // NoOp
    }
}

static EXIT: AtomicBool = AtomicBool::new(false);

fn set_exit() {
    EXIT.store(true, atomic::Ordering::Relaxed);
}

fn should_exit() -> bool {
    EXIT.load(atomic::Ordering::Relaxed)
}

extern "C" fn signal_handler(_signal: core::ffi::c_int) {
    set_exit();
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
            log::error!("Failed to install signal handler: {e}");
        }
    }

    action.sa_handler_kernel = sig_ign();
    if let Err(e) = unsafe { sigaction(Signal::CHILD, Some(action)) } {
        log::error!("Failed to install signal handler: {e}");
    }
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
        Err(e) => log::error!("fork failed: {e}"),
        _ => {}
    }
}
