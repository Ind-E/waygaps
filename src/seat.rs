use waybackend::types::ObjectId;

use crate::wayland::wl_pointer::Axis;

pub struct Pointer {
    pub id: ObjectId,

    pub button: u32,
    pub enter_serial: u32,
    pub value120: i32,
    pub axis: Axis,

    pub current_surface: u32,
    pub on_clickable: bool,
}

impl Pointer {
    pub fn new(id: ObjectId) -> Self {
        Self {
            id,
            button: 0,
            enter_serial: 0,
            value120: 0,
            axis: Axis::vertical_scroll,
            current_surface: u32::MAX,
            on_clickable: false,
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
