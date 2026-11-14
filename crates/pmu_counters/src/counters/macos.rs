//! The counters of macOS: the kernel keeps instructions and cycles of
//! each thread, and `proc_pid_rusage` sums them over the process, user
//! space and kernel together. They always count, so enabling and
//! disabling do nothing. A virtual machine reports zero.

use core::ffi::CStr;
use core::mem::MaybeUninit;

use super::{Backend, Counts};
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

impl Backend for Rusage {
    fn plan(&mut self, _: &CStr) -> Result<u32, Error> {
        Ok(0)
    }

    fn open(&mut self) -> Result<(), Error> {
        // The process has run millions of instructions by now; zero
        // means the kernel keeps no counters here.
        if self.read()?.instructions == 0 {
            return Err(Error::new(Status::NoCounters));
        }
        Ok(())
    }

    fn enable(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn disable(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn read(&mut self) -> Result<Counts, Error> {
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
        Ok(Counts {
            instructions: info.ri_instructions,
            cycles: info.ri_cycles,
        })
    }
}
