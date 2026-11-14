//! The calls of the system behind [`super::perf::Perf`] on Linux.

use core::ffi::{c_int, c_ulong};

use super::perf::PerfSys;
use super::perf_abi::{PERF_FLAG_FD_CLOEXEC, PerfEventAttr};

pub struct LinuxSys;

impl LinuxSys {
    pub const fn new() -> Self {
        LinuxSys
    }
}

impl Default for LinuxSys {
    fn default() -> Self {
        Self::new()
    }
}

fn errno() -> i32 {
    // SAFETY: __errno_location returns the address of the thread's errno.
    unsafe { *libc::__errno_location() }
}

impl PerfSys for LinuxSys {
    fn open(&mut self, attr: &PerfEventAttr, group_fd: i32) -> Result<i32, i32> {
        // SAFETY: attr is a PerfEventAttr whose size field says 64 bytes,
        // which it has; pid 0 and cpu -1 are this thread on any CPU.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                core::ptr::from_ref(attr),
                0 as libc::pid_t,
                -1 as c_int,
                group_fd as c_int,
                PERF_FLAG_FD_CLOEXEC as c_ulong,
            )
        };
        if fd < 0 {
            return Err(errno());
        }
        i32::try_from(fd).map_err(|_| libc::EINVAL)
    }

    fn ioctl(&mut self, fd: i32, request: u64, arg: u64) -> Result<(), i32> {
        // SAFETY: the requests of perf events take an integer argument.
        let rc = unsafe { libc::ioctl(fd, request as libc::Ioctl, arg as c_ulong) };
        if rc < 0 { Err(errno()) } else { Ok(()) }
    }

    fn read(&mut self, fd: i32, buf: &mut [u8]) -> Result<usize, i32> {
        // SAFETY: buf is writable for its length.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        usize::try_from(n).map_err(|_| errno())
    }

    fn close(&mut self, fd: i32) {
        // SAFETY: fd is a descriptor this crate opened.
        unsafe { libc::close(fd) };
    }

    #[cfg(target_arch = "x86_64")]
    fn uniform_cores(&self) -> Option<bool> {
        use core::arch::x86_64::{__cpuid_count, __get_cpuid_max};
        // A processor without leaf 7 is older than those with two kinds
        // of cores. Leaf 7 tells in bit 15 of EDX that it is hybrid.
        let (max, _) = __get_cpuid_max(0);
        if max < 7 {
            return Some(true);
        }
        let leaf = __cpuid_count(7, 0);
        Some(leaf.edx & (1 << 15) == 0)
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn uniform_cores(&self) -> Option<bool> {
        None
    }
}
