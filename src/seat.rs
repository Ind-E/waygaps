use waybackend::types::ObjectId;

use crate::wayland;

#[repr(u8)]
#[derive(Clone, Copy)]
pub enum Axis {
    Vertical = 0,
    // incorecctly marked as unused because it's
    // only created by transmute
    #[allow(unused)]
    Horizontal = 1,
}

impl From<wayland::wl_pointer::Axis> for Axis {
    #[inline]
    fn from(value: wayland::wl_pointer::Axis) -> Self {
        // SAFETY: wl_pointer::Axis is repr(u32) and has variants
        // in the same order
        unsafe { core::mem::transmute(value as u8) }
    }
}

#[repr(u8)]
pub enum AxisSource {
    #[allow(unused)]
    Wheel = 0,
    #[allow(unused)]
    Finger = 1,
    #[allow(unused)]
    Continuous = 2,
    #[allow(unused)]
    WheelTilt = 3,
    None = 4,
}

impl From<wayland::wl_pointer::AxisSource> for AxisSource {
    #[inline]
    fn from(value: wayland::wl_pointer::AxisSource) -> Self {
        // SAFETY: wl_pointer::AxisSource is repr(u32) and has variants
        // in the same order
        unsafe { core::mem::transmute(value as u8) }
    }
}

pub struct Pointer {
    pub id: ObjectId,
    pub relative_pointer_id: Option<ObjectId>,

    pub pressure_x: f64,
    pub pressure_y: f64,
    pub last_time: u64,
    pub last_trigger_time: u64,
    pub should_trigger_edge: bool,

    pub button: u16,
    pub enter_serial: u32,
    pub scroll: f64,
    pub scroll120: i32,
    pub axis: Axis,
    pub source: AxisSource,

    pub current_waygap_idx: u16,
}

impl Pointer {
    #[inline]
    pub const fn new(id: ObjectId) -> Self {
        Self {
            id,
            relative_pointer_id: None,

            pressure_x: 0.0,
            pressure_y: 0.0,
            last_time: 0,
            last_trigger_time: 0,
            should_trigger_edge: false,

            button: 0,
            enter_serial: 0,
            scroll: 0.0,
            scroll120: 0,
            axis: Axis::Vertical,
            source: AxisSource::None,

            current_waygap_idx: u16::MAX,
        }
    }
}

pub struct Seat {
    pub registry_name: u32,

    pub id: ObjectId,

    pub pointer: Option<Pointer>,
}

impl Seat {
    pub const fn new(registry_name: u32, wl_seat: ObjectId) -> Self {
        Self {
            registry_name,
            id: wl_seat,
            pointer: None,
        }
    }
}
