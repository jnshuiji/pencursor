//! Low-level Linux/Bionic system calls, dynamic linker (`dlopen`/`dlsym`), and kernel event primitives.
//!
//! Provides zero-dependency FFI bindings and RAII wrappers (`OwnedFd`, `DlLib`) for `epoll`,
//! `inotify`, `timerfd`, `ioctl`, and thread-level credential switching.

use std::ffi::{CStr, CString, c_char, c_int, c_long, c_ulong, c_void};
use std::io;
use std::ptr;

pub const CLOCK_MONOTONIC: c_int = 1;
pub const TFD_CLOEXEC: c_int = 0o2000000;
pub const TFD_NONBLOCK: c_int = 0o4000;

pub const IN_NONBLOCK: c_int = 0o4000;
pub const IN_CLOEXEC: c_int = 0o2000000;
pub const IN_MODIFY: u32 = 0x0000_0002;
pub const IN_ATTRIB: u32 = 0x0000_0004;
pub const IN_CLOSE_WRITE: u32 = 0x0000_0008;
pub const IN_MOVED_TO: u32 = 0x0000_0080;
pub const IN_CREATE: u32 = 0x0000_0100;
pub const IN_DELETE: u32 = 0x0000_0200;

pub const O_RDONLY: c_int = 0;
pub const O_NONBLOCK: c_int = 0o4000;
pub const O_CLOEXEC: c_int = 0o2000000;

pub const EPOLL_CLOEXEC: c_int = 0o2000000;
pub const EPOLL_CTL_ADD: c_int = 1;
pub const EPOLL_CTL_DEL: c_int = 2;
pub const EPOLLIN: u32 = 0x001;
pub const EPOLLERR: u32 = 0x008;
pub const EPOLLHUP: u32 = 0x010;

pub const RTLD_NOW: c_int = 2;
pub const RTLD_LOCAL: c_int = 0;

pub const PR_SET_NAME: c_int = 15;

#[cfg(target_arch = "aarch64")]
pub const SYS_SETRESUID: c_long = 147;
#[cfg(not(target_arch = "aarch64"))]
pub const SYS_SETRESUID: c_long = 117;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Itimerspec {
    pub it_interval: Timespec,
    pub it_value: Timespec,
}

#[cfg_attr(target_arch = "x86_64", repr(C, packed))]
#[cfg_attr(not(target_arch = "x86_64"), repr(C))]
#[derive(Debug, Clone, Copy, Default)]
pub struct EpollEvent {
    pub events: u32,
    pub u64_data: u64,
}

unsafe extern "C" {
    pub fn open(pathname: *const c_char, flags: c_int, ...) -> c_int;
    pub fn close(fd: c_int) -> c_int;
    pub fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    pub fn timerfd_create(clockid: c_int, flags: c_int) -> c_int;
    pub fn timerfd_settime(
        fd: c_int,
        flags: c_int,
        new_value: *const Itimerspec,
        old_value: *mut Itimerspec,
    ) -> c_int;
    pub fn inotify_init1(flags: c_int) -> c_int;
    pub fn inotify_add_watch(fd: c_int, pathname: *const c_char, mask: u32) -> c_int;
    pub fn epoll_create1(flags: c_int) -> c_int;
    pub fn epoll_ctl(epfd: c_int, op: c_int, fd: c_int, event: *mut EpollEvent) -> c_int;
    pub fn epoll_wait(
        epfd: c_int,
        events: *mut EpollEvent,
        maxevents: c_int,
        timeout: c_int,
    ) -> c_int;
    pub fn syscall(num: c_long, ...) -> c_long;
    pub fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    pub fn dlclose(handle: *mut c_void) -> c_int;
    pub fn prctl(option: c_int, ...) -> c_int;
}

/// RAII wrapper around an owned POSIX file descriptor.
#[derive(Debug)]
pub struct OwnedFd(c_int);

impl OwnedFd {
    /// Creates an [`OwnedFd`] taking ownership of `fd`.
    ///
    /// # Safety
    /// `fd` must be a valid open file descriptor exclusively owned by the caller.
    #[inline]
    pub const unsafe fn from_raw(fd: c_int) -> Self {
        Self(fd)
    }

    /// Returns the underlying raw file descriptor.
    #[inline]
    pub const fn raw(&self) -> c_int {
        self.0
    }
}

impl Drop for OwnedFd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            // SAFETY: `self.0` is an open file descriptor owned by this struct.
            unsafe {
                let _ = close(self.0);
            }
        }
    }
}

/// RAII wrapper around a dynamically loaded shared library handle (`dlopen` / `dlclose`).
#[derive(Debug)]
pub struct DlLib {
    handle: *mut c_void,
}

// SAFETY: Dynamic library handles loaded via `dlopen` can be safely shared across threads.
unsafe impl Send for DlLib {}
// SAFETY: Symbol lookups via `dlsym` are thread-safe under Bionic/glibc.
unsafe impl Sync for DlLib {}

impl DlLib {
    /// Opens a shared library at `path` with `RTLD_NOW | RTLD_LOCAL`.
    pub fn open(path: &str) -> Result<Self, String> {
        let c_path = CString::new(path).map_err(|_| format!("invalid path: {path}"))?;
        // SAFETY: `c_path` is a valid NUL-terminated string.
        let handle = unsafe { dlopen(c_path.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        if handle.is_null() {
            return Err(format!("dlopen({path}) failed"));
        }
        Ok(Self { handle })
    }

    /// Resolves a symbol address by name.
    #[inline]
    pub fn sym(&self, name: &str) -> Option<usize> {
        let c_name = CString::new(name).ok()?;
        self.sym_cstr(&c_name)
    }

    /// Resolves a symbol address by `&CStr`.
    #[inline]
    pub fn sym_cstr(&self, name: &CStr) -> Option<usize> {
        // SAFETY: `self.handle` is a valid `dlopen` handle and `name` is NUL-terminated.
        let ptr = unsafe { dlsym(self.handle, name.as_ptr()) };
        if ptr.is_null() {
            None
        } else {
            Some(ptr as usize)
        }
    }
}

impl Drop for DlLib {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `self.handle` was returned by a successful `dlopen` call.
            unsafe {
                let _ = dlclose(self.handle);
            }
        }
    }
}

/// Switches the calling thread's effective UID (`SYS_setresuid(-1, euid, -1)`).
#[inline]
pub fn set_thread_euid(euid: u32) -> bool {
    // SAFETY: Direct `SYS_setresuid` syscall modifying only the current thread's effective UID.
    let rc = unsafe { syscall(SYS_SETRESUID, -1i32, euid as c_int, -1i32) };
    rc == 0
}

/// Arms a `timerfd` for a single one-shot expiration after `timeout_ms` milliseconds.
pub fn arm_oneshot_timerfd_ms(tfd: c_int, timeout_ms: u64) -> io::Result<()> {
    let sec = (timeout_ms / 1_000) as i64;
    let nsec = ((timeout_ms % 1_000) * 1_000_000).max(1) as i64;
    let spec = Itimerspec {
        it_interval: Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: Timespec {
            tv_sec: sec,
            tv_nsec: nsec,
        },
    };
    // SAFETY: `spec` is a valid stack-allocated `Itimerspec`.
    let ret = unsafe { timerfd_settime(tfd, 0, &spec, ptr::null_mut()) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Disarms an active `timerfd`.
pub fn disarm_timerfd(tfd: c_int) {
    let spec = Itimerspec::default();
    // SAFETY: All-zero `Itimerspec` disarms the timerfd.
    unsafe {
        let _ = timerfd_settime(tfd, 0, &spec, ptr::null_mut());
    }
}

/// Drains the 8-byte expiration counter from a `timerfd`.
#[inline]
pub fn drain_timerfd(fd: c_int) -> u64 {
    let mut val = 0u64;
    // SAFETY: Reads an 8-byte `u64` counter into a valid stack variable.
    let n = unsafe { read(fd, (&raw mut val).cast(), size_of::<u64>()) };
    if n == size_of::<u64>() as isize {
        val
    } else {
        0
    }
}

/// Registers `fd` with the `epoll` instance `ep_fd`.
pub fn epoll_add(ep_fd: c_int, fd: c_int, events: u32, tag: u64) -> io::Result<()> {
    let mut ev = EpollEvent {
        events,
        u64_data: tag,
    };
    // SAFETY: `ev` is a valid `EpollEvent` passed to `epoll_ctl`.
    let ret = unsafe { epoll_ctl(ep_fd, EPOLL_CTL_ADD, fd, &mut ev) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Removes `fd` from the `epoll` instance `ep_fd`.
pub fn epoll_del(ep_fd: c_int, fd: c_int) {
    // SAFETY: `EPOLL_CTL_DEL` ignores the event pointer.
    unsafe {
        let _ = epoll_ctl(ep_fd, EPOLL_CTL_DEL, fd, ptr::null_mut());
    }
}

/// Sets the calling thread/process comm name via `prctl(PR_SET_NAME)`.
#[inline]
pub fn set_process_name(name: &str) {
    let mut buf = [0u8; 16];
    let bytes = name.as_bytes();
    let n = bytes.len().min(15);
    buf[..n].copy_from_slice(&bytes[..n]);
    buf[n] = 0;
    // SAFETY: `buf` is a 16-byte NUL-terminated buffer as required by `PR_SET_NAME`.
    unsafe {
        let _ = prctl(PR_SET_NAME, buf.as_ptr() as c_ulong, 0, 0, 0);
    }
}
