#![no_std]
#![no_main]
#![feature(cstr_display)]

extern crate alloc;

use core::{
    mem::MaybeUninit,
    sync::atomic::{self, AtomicBool},
};

use rustix::{
    self, event::epoll, process::{self}
};
use smallvec::SmallVec;
use waybackend::{
    Waybackend,
    objman::{self, ObjectManager},
    types::{ObjectId, WlFixed},
};

use crate::{
    config::{Anchor, Config, GapConfig, InputEvent, read_config},
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
        // group of 8KB, so that we need only 1 allocation in the average case,
        // and use (hopefully) 2 pages of memory
        let len = (layout.size() + 256).next_multiple_of(8192);

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
                log::debug!("new allocation of size: {len}");
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
    argc: core::ffi::c_int,
    argv: *const *const i8,
    envp: *const *const i8,
) -> core::ffi::c_int {
    #[cfg(feature = "schema")]
    {
        use rustix::fs::{Mode, OFlags};
        use schemars::schema_for;

        let schema = schema_for!(Config);
        let json = serde_json::to_string_pretty(&schema).unwrap();

        const PATH: &'static str =
            concat!(env!("CARGO_MANIFEST_DIR"), "/schema.json");

        let fd = rustix::fs::open(
            PATH,
            OFlags::CREATE | OFlags::WRONLY | OFlags::TRUNC,
            Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH,
        )
        .unwrap();
        rustix::io::write(fd, json.as_bytes()).unwrap();
        return 0;
    }
    unsafe { environ = envp.cast() };

    #[cfg(not(debug_assertions))]
    log::init(log::Filter::Info);
    #[cfg(debug_assertions)]
    log::init(log::Filter::Debug);

    let args = parse_args(argc, argv);

    // lower our process niceness priority. It's ok to delay updating the gaps
    // if the system is under heavy load
    let _ = rustix::process::nice(1);

    let config = read_config(args.config_path);

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

    let mut app = App::new(backend, objman, config, args.preview);

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

struct App {
    backend: waybackend::Waybackend,
    objman: objman::ObjectManager<WaylandObject>,
    registry: ObjectId,
    compositor: ObjectId,
    shm: ObjectId,
    layer_shell: ObjectId,
    relative_ptr_mgr: ObjectId,
    waygaps: alloc::vec::Vec<WayGap>,
    seats: alloc::vec::Vec<Seat>,
    pending_outputs: alloc::vec::Vec<PendingOutput>,
    config: Config,

    preview: bool,
}

impl App {
    fn new(
        backend: waybackend::Waybackend,
        objman: objman::ObjectManager<WaylandObject>,
        config: Config,
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
            waygaps: alloc::vec::Vec::new(),
            seats: alloc::vec::Vec::new(),
            pending_outputs: alloc::vec::Vec::new(),
            config,
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

// not a method on App to avoid borrowing shenanigans
fn create_waygap(
    backend: &mut Waybackend,
    objman: &mut ObjectManager<WaylandObject>,
    compositor: ObjectId,
    layer_shell: ObjectId,
    registry_name: u32,
    wl_output: ObjectId,
    config: &GapConfig,
    waygaps: &mut alloc::vec::Vec<WayGap>,
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

    fn delete_id(&mut self, _: ObjectId, id: u32) {
        self.objman.remove(id);
    }
}

impl wayland::wl_registry::EvHandler for App {
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
    fn done(&mut self, _sender_id: ObjectId, _: u32) {
        // NoOp (needed for rountrip)
    }
}

impl wayland::wl_compositor::EvHandler for App {}

impl wayland::wl_seat::EvHandler for App {
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

    fn name(&mut self, _: ObjectId, _: &str) {
        // NoOp
    }
}

impl wayland::wl_shm::EvHandler for App {
    fn format(&mut self, _: ObjectId, _: wayland::wl_shm::Format) {
        // ignore all messages since we will simply use ARGB8888 every time
    }
}

impl wayland::wl_shm_pool::EvHandler for App {}

impl wayland::wl_buffer::EvHandler for App {
    fn release(&mut self, sender_id: ObjectId) {
        wayland::wl_buffer::req::destroy(&mut self.backend, sender_id).unwrap();
    }
}

impl wayland::wl_surface::EvHandler for App {
    fn enter(&mut self, _sender_id: ObjectId, _output: ObjectId) {
        // NoOp
    }

    fn leave(&mut self, _sender_id: ObjectId, _output: ObjectId) {
        // NoOp
    }

    fn preferred_buffer_scale(&mut self, _sender_id: ObjectId, _factor: i32) {
        // NoOp
    }

    fn preferred_buffer_transform(
        &mut self,
        _sender_id: ObjectId,
        _transform: wayland::wl_output::Transform,
    ) {
        // NoOp
    }
}

impl wayland::wl_output::EvHandler for App {
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

        for (cfg_name, cfg) in self.config.iter() {
            if is_output_match(cfg.output.as_deref(), output.description) {
                log::info!(
                    "opening config `{}` on output `{}`",
                    cfg_name,
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

    fn scale(&mut self, _sender_id: ObjectId, _factor: i32) {
        // NoOp
    }

    fn name(&mut self, sender_id: ObjectId, name: &str) {
        if let Some(out) =
            self.pending_outputs.iter_mut().find(|o| o.id == sender_id)
        {
            let static_name = unsafe { core::mem::transmute(name) };
            out.name = static_name;
        }
    }

    fn description(&mut self, sender_id: ObjectId, description: &str) {
        if let Some(out) =
            self.pending_outputs.iter_mut().find(|o| o.id == sender_id)
        {
            // this probably doesn't segfault if more than output is connected
            let static_description =
                unsafe { core::mem::transmute(description) };
            out.description = static_description;
        }
    }
}

impl wayland::wl_pointer::EvHandler for App {
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

    fn motion(
        &mut self,
        _sender_id: ObjectId,
        _time: u32,
        _surface_x: WlFixed,
        _surface_y: WlFixed,
    ) {
        // NoOp
    }

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

    fn axis(
        &mut self,
        sender_id: ObjectId,
        _time: u32,
        axis: wayland::wl_pointer::Axis,
        value: WlFixed,
    ) {
        let ptr = match get_pointer(&mut self.seats, sender_id) {
            Some(ptr) => match self.waygaps.get(ptr.current_waygap as usize) {
                Some(_waygap) => ptr,
                None => return,
            },
            None => return,
        };

        ptr.axis = axis.into();
        ptr.scroll += f64::from(value);
    }

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

        if ptr.scroll <= -15.0 {
            ptr.scroll += 15.0;
            for event in waygap.commands.iter() {
                if let (InputEvent::Scroll(scroll), cmd) = event
                    && scroll.on_axis(ptr.axis)
                    && !scroll.is_positive()
                {
                    shell_command(cmd);
                }
            }
        } else if ptr.scroll >= 15.0 {
            ptr.scroll -= 15.0;
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

    fn axis_source(
        &mut self,
        _sender_id: ObjectId,
        _axis_source: wayland::wl_pointer::AxisSource,
    ) {
        // NoOp
    }

    fn axis_stop(
        &mut self,
        _sender_id: ObjectId,
        _time: u32,
        _axis: wayland::wl_pointer::Axis,
    ) {
        // NoOp
    }

    fn axis_discrete(
        &mut self,
        _sender_id: ObjectId,
        _axis: wayland::wl_pointer::Axis,
        _discrete: i32,
    ) {
    }

    fn axis_value120(
        &mut self,
        _sender_id: ObjectId,
        _axis: wayland::wl_pointer::Axis,
        _value120: i32,
    ) {
    }

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

        if dt > 400 {
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
            if time - ptr.last_trigger_time > 250000 {
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
fn shell_command(command: &core::ffi::CStr) {
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
