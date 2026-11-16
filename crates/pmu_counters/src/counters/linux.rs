//! The calls of the system behind [`super::perf::Perf`] on Linux.

use core::ffi::{CStr, c_int, c_ulong};

use super::perf::{PerfSys, parse_tid};
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
    fn open(&mut self, attr: &PerfEventAttr, tid: i32, group_fd: i32) -> Result<i32, i32> {
        // SAFETY: attr is a PerfEventAttr whose size field says 64 bytes,
        // which it has; a thread's id and cpu -1 are that thread on any
        // CPU.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                core::ptr::from_ref(attr),
                tid as libc::pid_t,
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

    fn threads(&mut self, out: &mut [i32]) -> Result<usize, i32> {
        // SAFETY: the path is a string ending in zero.
        let dir = unsafe { libc::opendir(c"/proc/self/task".as_ptr()) };
        if dir.is_null() {
            return Err(errno());
        }
        let mut n = 0usize;
        let listed = loop {
            // readdir returns null at the end and on an error, and sets
            // errno only on an error.
            // SAFETY: __errno_location returns the address of the
            // thread's errno.
            unsafe { *libc::__errno_location() = 0 };
            // SAFETY: dir is an open directory stream.
            let entry = unsafe { libc::readdir(dir) };
            if entry.is_null() {
                let e = errno();
                break if e == 0 { Ok(n) } else { Err(e) };
            }
            // SAFETY: the entry readdir returned stays valid until the
            // next call, and its name ends in zero.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if let Some(tid) = parse_tid(name.to_bytes()) {
                if let Some(slot) = out.get_mut(n) {
                    *slot = tid;
                }
                n = n.saturating_add(1);
            }
        };
        // SAFETY: dir is open, and closed once.
        unsafe { libc::closedir(dir) };
        listed
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
