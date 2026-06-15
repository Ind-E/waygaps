use core::{
    fmt::{self, Write as _},
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};

static IS_TTY: AtomicBool = AtomicBool::new(false);
static FILTER: AtomicU8 = AtomicU8::new(Filter::Fatal as u8);

#[cfg(debug_assertions)]
pub const MIN_LEVEL: Filter = Filter::Debug;

#[cfg(not(debug_assertions))]
pub const MIN_LEVEL: Filter = Filter::Info;

#[repr(u8)]
#[derive(Clone, Copy)]
pub enum Filter {
    #[cfg(debug_assertions)]
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
    Fatal = 4,
}

struct Stderr;
impl fmt::Write for Stderr {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let fd = unsafe { rustix::stdio::stderr() };
        write_all(fd, s.as_bytes())
    }
}

struct Stdout;
impl fmt::Write for Stdout {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let fd = unsafe { rustix::stdio::stdout() };
        write_all(fd, s.as_bytes())
    }
}

fn write_all(fd: rustix::fd::BorrowedFd<'_>, mut bytes: &[u8]) -> fmt::Result {
    while !bytes.is_empty() {
        match rustix::io::write(fd, bytes) {
            Ok(n) => bytes = &bytes[n..],
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return Err(fmt::Error),
        }
    }
    Ok(())
}

pub fn init(filter: Filter) {
    let stderr = unsafe { rustix::stdio::stderr() };
    IS_TTY.store(rustix::termios::isatty(stderr), Ordering::SeqCst);
    FILTER.store(filter as u8, Ordering::SeqCst);
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    let _ = Stdout.write_fmt(args);
}

#[cold]
#[inline(never)]
pub fn log(filter: Filter, msg: core::fmt::Arguments) {
    if (filter as u8) < FILTER.load(Ordering::Relaxed) {
        return;
    }

    let mut writer = Stderr;

    let level = if IS_TTY.load(Ordering::Relaxed) {
        match filter {
            Filter::Fatal => "\x1b[30;47m[FATAL]\x1b[0m ",
            Filter::Error => "\x1b[31m[ERROR]\x1b[0m ",
            Filter::Warn => "\x1b[33m[WARN]\x1b[0m  ",
            Filter::Info => "\x1b[32m[INFO]\x1b[0m  ",
            #[cfg(debug_assertions)]
            Filter::Debug => "\x1b[36m[DEBUG]\x1b[0m ",
        }
    } else {
        match filter {
            Filter::Fatal => "[FATAL] ",
            Filter::Error => "[ERROR] ",
            Filter::Warn => "[WARN]  ",
            Filter::Info => "[INFO]  ",
            #[cfg(debug_assertions)]
            Filter::Debug => "[DEBUG] ",
        }
    };
    let _ = writer.write_str(level);
    let _ = writer.write_fmt(msg);
    let _ = writer.write_str("\n");
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::log::_print(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {
        $crate::log::_print(format_args!("{}\n", format_args!($($arg)*)))
    };
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

// #[cfg(debug_assertions)]
// pub use _debug as debug;
pub use _error as error;
pub use _fatal as fatal;
pub use _info as info;
pub use _warn as warn;
