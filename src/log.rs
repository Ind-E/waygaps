use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use ::alloc::string::ToString as _;

static IS_TTY: AtomicBool = AtomicBool::new(false);
static FILTER: AtomicU8 = AtomicU8::new(Filter::Fatal as u8);

#[cfg(debug_assertions)]
pub const MIN_LEVEL: Filter = Filter::Trace;

#[cfg(not(debug_assertions))]
pub const MIN_LEVEL: Filter = Filter::Info;

#[repr(u8)]
#[derive(Clone, Copy)]
pub enum Filter {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
    Fatal = 5,
}

pub fn init(filter: Filter) {
    // this is unsafe because in no-std environments the stderr file descriptor
    // may be invalid
    #[allow(unused_unsafe)]
    let stderr = unsafe { rustix::stdio::stderr() };
    IS_TTY.store(rustix::termios::isatty(stderr), Ordering::SeqCst);
    FILTER.store(filter as u8, Ordering::SeqCst);
}

#[cold]
#[inline(never)]
pub fn log(filter: Filter, msg: core::fmt::Arguments) {
    if (filter as u8) < FILTER.load(Ordering::Relaxed) {
        return;
    }

    #[rustfmt::skip]
    let level = if IS_TTY.load(Ordering::Relaxed) {
        match filter {
            Filter::Fatal => "\x1b[30;47m[FATAL]\x1b[0m ",
            Filter::Error => "\x1b[31m[ERROR]\x1b[0m ",
            Filter::Warn =>  "\x1b[33m[WARN]\x1b[0m  ",
            Filter::Info =>  "\x1b[32m[INFO]\x1b[0m  ",
            Filter::Debug => "\x1b[36m[DEBUG]\x1b[0m ",
            Filter::Trace => "[TRACE] ",
        }
    } else {
        match filter {
            Filter::Fatal => "[FATAL] ",
            Filter::Error => "[ERROR] ",
            Filter::Warn =>  "[WARN]  ",
            Filter::Info =>  "[INFO]  ",
            Filter::Debug => "[DEBUG] ",
            Filter::Trace => "[TRACE] ",
        }
    };

    let msg = match msg.as_str() {
        Some(s) => ::alloc::borrow::Cow::Borrowed(s),
        None => ::alloc::borrow::Cow::Owned(msg.to_string()),
    };

    // this is unsafe because in no-std environments the stderr file descriptor
    // may be invalid
    #[allow(unused_unsafe)]
    let stderr = unsafe { rustix::stdio::stderr() };
    let bufs = [
        rustix::io::IoSlice::new(level.as_bytes()),
        rustix::io::IoSlice::new(msg.as_bytes()),
        rustix::io::IoSlice::new(b"\n"),
    ];
    _ = rustix::io::writev(stderr, &bufs);
}

#[macro_export]
macro_rules! _trace {
    ($($arg:tt)+) => {
        if const { $crate::log::MIN_LEVEL as u8 <= $crate::log::Filter::Trace as u8 } {
            $crate::log::log($crate::log::Filter::Trace, format_args!($($arg)+))
        }
    }
}

#[macro_export]
macro_rules! _debug {
    ($($arg:tt)+) => {
        if const { $crate::log::MIN_LEVEL as u8 <= $crate::log::Filter::Debug as u8 }  {
            $crate::log::log($crate::log::Filter::Debug, format_args!($($arg)+))
        }
    }
}

#[macro_export]
macro_rules! _info {
    ($($arg:tt)+) => {
        if const { $crate::log::MIN_LEVEL as u8 <= $crate::log::Filter::Info as u8 } {
            $crate::log::log($crate::log::Filter::Info, format_args!($($arg)+))
        }
    }
}

#[macro_export]
macro_rules! _warn {
    ($($arg:tt)+) => {
        if const { $crate::log::MIN_LEVEL as u8 <= $crate::log::Filter::Warn as u8 } {
            $crate::log::log($crate::log::Filter::Warn, format_args!($($arg)+))
        }
    }
}

#[macro_export]
macro_rules! _error {
    ($($arg:tt)+) => {
        if const { $crate::log::MIN_LEVEL as u8 <= $crate::log::Filter::Error as u8 } {
            $crate::log::log($crate::log::Filter::Error, format_args!($($arg)+))
        }
    }
}

#[macro_export]
macro_rules! _fatal {
    ($($arg:tt)+) => {
        if const { $crate::log::MIN_LEVEL as u8 <= $crate::log::Filter::Fatal as u8 } {
            $crate::log::log($crate::log::Filter::Fatal, format_args!($($arg)+))
        }
    }
}

pub use _debug as debug;
pub use _error as error;
pub use _fatal as fatal;
pub use _info as info;
pub use _trace as trace;
pub use _warn as warn;
