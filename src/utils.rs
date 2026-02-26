use core::ffi::CStr;
use core::ffi::c_int;

use smallvec::SmallVec;
use smallvec::ToSmallVec as _;
use waybackend::{Waybackend, objman::ObjectManager, wire::Receiver};

use crate::log;

/// Manual getenv implementation from an extern environ variable.
///
/// Note: this is marked as `#[inline(never)]` and `#[cold]` because this
/// function will be executed a few times, mostly during initialization, and
/// thus inlining it would serve little purpose other than increasing binary
/// size.
///
/// Note2: we do not use `libc::getenv` because the long-term plan is not
/// depending on `libc` in the `daemon` (currently we can only do that in Rust
/// nightly).
///
/// # Safety
///
/// The `env` parameter must **NOT** end with an `=` byte (before the final null
/// byte, of course).
#[cold]
#[inline(never)]
pub unsafe fn getenv(env: &[u8]) -> Option<&CStr> {
    unsafe extern "C" {
        static environ: *const *const core::ffi::c_char;
    }

    let mut ptr = unsafe { environ };
    loop {
        let cptr = unsafe { ptr.read() };
        if cptr.is_null() {
            return None;
        }
        // SAFETY: environ is composed of null terminated strings, so this
        // should be safe
        let cstr = unsafe { core::ffi::CStr::from_ptr(cptr) };
        if let Some(value) = cstr.to_bytes_with_nul().strip_prefix(env) {
            // SAFETY:
            // Because `env` does not end with a `=` byte, value[1..] will
            // always skip the `=` byte, and the rest of the string
            // is guaranteed to end in a null byte, since it was
            // created by removing the prefix of another CStr, which would also
            // ends in a null byte
            return Some(unsafe {
                CStr::from_bytes_with_nul_unchecked(&value[1..])
            });
        }
        ptr = unsafe { ptr.add(1) };
    }
}

#[inline]
pub fn connect<T>(display: T) -> (Waybackend, ObjectManager<T>, Receiver)
where
    T: Copy + PartialEq,
{
    use rustix::fd::{FromRawFd, OwnedFd};
    use rustix::net::AddressFamily;

    if let Some(txt) = unsafe { getenv(b"WAYLAND_SOCKET") } {
        // We should connect to the provided WAYLAND_SOCKET
        let fd = parse_cstr_to_rawfd(txt)
            .expect("file descriptor in WAYLAND_SOCKET is not a number");

        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let socket_addr =
            rustix::net::getsockname(&fd).expect("failed to getsocketname");
        if socket_addr.address_family() == AddressFamily::UNIX {
            unsafe { waybackend::connect_from_fd(display, fd) }
        } else {
            panic!(
                "Socket in WAYLAND_SOCKET has wrong family: {}",
                socket_addr.address_family().as_raw()
            );
        }
    } else {
        let socket_name =
            unsafe { getenv(b"WAYLAND_DISPLAY") }.unwrap_or_else(|| {
                log::warn!(
                    "WAYLAND_DISPLAY is not set! Defaulting to wayland-0"
                );
                c"wayland-0"
            });

        let unix_addr = if socket_name.to_bytes().first() == Some(&b'/') {
            rustix::net::SocketAddrUnix::new(socket_name).unwrap()
        } else {
            let mut socket_fullpath = if let Some(socket_path) =
                unsafe { getenv(b"XDG_RUNTIME_DIR") }
            {
                socket_path.to_bytes().to_smallvec()
            } else {
                use rustix::path::DecInt;
                log::warn!(
                    "XDG_RUNTIME_DIR is not set! Defaulting to /run/user/UID"
                );
                let mut v = SmallVec::<[u8; 16]>::from_slice(b"/run/user/");
                let uid = rustix::process::getuid();
                v.extend_from_slice(DecInt::new(uid.as_raw()).as_bytes());
                v
            };
            socket_fullpath.push(b'/');
            socket_fullpath.extend_from_slice(socket_name.to_bytes_with_nul());
            let path = unsafe {
                core::ffi::CStr::from_bytes_with_nul_unchecked(&socket_fullpath)
            };
            rustix::net::SocketAddrUnix::new(path).unwrap()
        };

        let socket = rustix::net::socket_with(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::STREAM,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )
        .expect("failed to create socket");

        waybackend::connect_to(display, socket, &unix_addr)
            .expect("failed to connect to socket")
    }
}

/// This function is unlikely to run, as most wayland implementations use
/// WAYLAND_DISPLAY, not WAYLAND_SOCKET
///
/// We are writting our own manual implementation because Rust cannot parse a
/// `cstr` directly. Instead, it demands we first transform it to a str (which
/// goes through a utf8 verification), and THEN try parsing the number,
/// therefore generating code with 2 unwraps and panic conditions, even though 1
/// would suffice
#[cold]
const fn parse_cstr_to_rawfd(s: &core::ffi::CStr) -> Option<rustix::fd::RawFd> {
    let mut fd: rustix::fd::RawFd = 0;
    let mut ptr = s.as_ptr();

    loop {
        let x = unsafe { ptr.read() } as core::ffi::c_int;
        if x == 0 {
            break;
        } else if x < b'0' as core::ffi::c_int || x > b'9' as core::ffi::c_int {
            return None;
        }
        fd = fd * 10 + (x - b'0' as core::ffi::c_int);
        ptr = unsafe { ptr.add(1) };
    }

    Some(fd)
}

#[inline]
pub fn is_output_match(pattern: Option<&str>, description: &str) -> bool {
    let Some(pattern) = pattern else {
        return true;
    };

    let output_matched = description.contains(pattern);

    if !output_matched {
        log::warn!("no outputs matched pattern `{pattern}`");
    }

    output_matched
}

pub struct Args {
    pub preview: bool,
    pub config_path: Option<&'static CStr>,
}

#[inline]
pub fn parse_args(argc: c_int, argv: *const *const i8) -> Args {
    let mut args = Args {
        preview: false,
        config_path: None,
    };

    let argv_slice =
        unsafe { core::slice::from_raw_parts(argv, argc as usize) };

    let mut i = 1;
    while i < argv_slice.len() {
        let ptr = argv_slice[i];
        if ptr.is_null() {
            i += 1;
            continue;
        }

        // 2. Wrap the pointer in a &CStr (Zero Allocation)
        let arg = unsafe { CStr::from_ptr(ptr) };

        match arg.to_bytes() {
            b"-p" | b"--preview" => {
                args.preview = true;
            }
            b"-v" | b"--version" => {
                log::info!("waygaps v{}", env!("CARGO_PKG_VERSION"));
                origin::program::exit(0);
            }
            b"-h" | b"--help" => help(0),
            b"-c" | b"--config" => {
                if i + 1 < argv_slice.len() {
                    let path_ptr = argv_slice[i + 1];
                    if !path_ptr.is_null() {
                        args.config_path =
                            Some(unsafe { CStr::from_ptr(path_ptr) });
                        i += 1;
                    }
                } else {
                    log::error!(
                        "a value is required for '{} <CONFIG>' but none was supplied",
                        arg.to_string_lossy()
                    );
                    origin::program::exit(-1);
                }
            }
            _ => {
                log::error!(
                    "unexpected argument '{}' found",
                    arg.to_string_lossy()
                );
                help(1);
            }
        }
        i += 1;
    }

    args
}

fn help(status: c_int) {
    log::info!("Usage: waygaps [OPTIONS]");
    log::info!("Options:");
    log::info!(
        "-c, --config <CONFIG>  Config file path (default: ~/.config/waygaps/config.toml)"
    );
    log::info!("-p, --preview          Preview the gaps on your screen(s)");
    log::info!("-h, --help             Print help");
    log::info!("-v, --version          Print version");
    origin::program::exit(status);
}
