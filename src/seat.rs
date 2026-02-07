use waybackend::types::ObjectId;

pub struct Pointer {
    pub id: ObjectId,
    pub cursor_device: ObjectId,

    pub x: u32,
    pub y: u32,
    pub button: u32,
    pub enter_serial: u32,
    pub scroll: i32,

    pub current_surface: u32,
    pub on_clickable: bool,
}

impl Pointer {
    pub fn new(id: ObjectId, cursor_device: ObjectId) -> Self {
        Self {
            id,
            cursor_device,
            x: 0,
            y: 0,
            button: 0,
            enter_serial: 0,
            scroll: 0,
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
