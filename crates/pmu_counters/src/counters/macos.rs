//! The counters of macOS: the kernel keeps instructions and cycles of
//! each thread, and `proc_pid_rusage` sums them over the process, user
//! space and kernel together. They always count, so enabling and
//! disabling do nothing. A virtual machine reports zero. The other sets
//! need the counters of the processor, which macOS gives only to root.

use core::ffi::CStr;
use core::mem::MaybeUninit;

use super::{Backend, Set};
use crate::{Error, Status};

pub struct Rusage;

impl Rusage {
    pub const fn new() -> Self {
        Rusage
    }
}

impl Default for Rusage {
    fn default() -> Self {
        Self::new()
    }
}

fn errno() -> i32 {
    // SAFETY: __error returns the address of the thread's errno.
    unsafe { *libc::__error() }
}

impl Rusage {
    fn rusage(&self) -> Result<(u64, u64), Error> {
        let mut info = MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: the buffer is a rusage_info_v4, as RUSAGE_INFO_V4
        // asks; the kernel fills it on success.
        let rc = unsafe {
            libc::proc_pid_rusage(
                libc::getpid(),
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast::<libc::rusage_info_t>(),
            )
        };
        if rc != 0 {
            return Err(Error::os(Status::SystemError, errno()));
        }
        // SAFETY: zeroed, then filled by the kernel.
        let info = unsafe { info.assume_init() };
        Ok((info.ri_instructions, info.ri_cycles))
    }
}

impl Backend for Rusage {
    fn plan(&mut self, _: &CStr, set: Set) -> Result<u32, Error> {
        match set {
            Set::Basic => Ok(0),
            _ => Err(Error::new(Status::EventsNotCounted)),
        }
    }

    fn open(&mut self, set: Set) -> Result<(), Error> {
        if set != Set::Basic {
            return Err(Error::new(Status::EventsNotCounted));
        }
        // The process has run millions of instructions by now; zero
        // means the kernel keeps no counters here.
        if self.rusage()?.0 == 0 {
            return Err(Error::new(Status::NoCounters));
        }
        Ok(())
    }

    fn enable(&mut self, _: Set) -> Result<(), Error> {
        Ok(())
    }

    fn disable(&mut self, _: Set) -> Result<(), Error> {
        Ok(())
    }

    fn read(&mut self, set: Set) -> Result<(u64, u64), Error> {
        match set {
            Set::Basic => self.rusage(),
            _ => Err(Error::new(Status::Internal)),
        }
    }
}
