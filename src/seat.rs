use waybackend::types::ObjectId;

use crate::wayland::wl_pointer::Axis;

#[derive(Clone, Debug)]
pub struct Pointer {
    pub id: ObjectId,

    pub last_x: i32,
    pub last_y: i32,
    pub pressure_x: i32,
    pub pressure_y: i32,
    pub last_time: u32,
    pub should_trigger_edge: bool,

    pub button: u32,
    pub enter_serial: u32,
    /// scroll amount. 120 is a normal scroll amount for one mouse ratchet
    pub value120: i32,
    pub axis: Axis,

    pub current_waygap: u32,
}

impl Pointer {
    pub fn new(id: ObjectId) -> Self {
        Self {
            id,

            last_x: 0,
            last_y: 0,
            pressure_x: 0,
            pressure_y: 0,
            last_time: 0,
            should_trigger_edge: false,

            button: 0,
            enter_serial: 0,
            value120: 0,
            axis: Axis::vertical_scroll,
            current_waygap: u32::MAX,
        }
    }
}

pub struct Seat {
    pub registry_name: u32,

    pub wl_seat: ObjectId,

    pub pointer: Option<Pointer>,
}

impl Seat {
    pub fn new(registry_name: u32, wl_seat: ObjectId) -> Self {
        Self {
            registry_name,
            wl_seat,
            pointer: None,
        }
    }
}
