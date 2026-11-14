//! The counters of instructions and cycles of one process.
//!
//! A [`Backend`] is the system's way to count: perf events on Linux,
//! the counters of the process on macOS, or none. [`Counters`] keeps
//! the order of the steps and the references: every counting statement
//! and every counting execution of a worker takes a reference, the
//! counters are enabled while at least one is held, and the values
//! only grow while they are enabled.

#[cfg(all(target_os = "macos", not(feature = "none")))]
mod macos;
// Unused where the system counts, but always built so that it stays right.
#[cfg_attr(all(target_os = "macos", not(feature = "none")), allow(dead_code))]
mod none;

use core::ffi::CStr;

use crate::{Error, Status};

/// Instructions and cycles counted so far. Only differences of two
/// readings mean something.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub instructions: u64,
    pub cycles: u64,
}

/// The system's way to count. Every function returns an error rather
/// than panic.
pub trait Backend {
    /// Find what must be opened, and return how many file descriptors
    /// the opening needs. It may be called again while nothing is open.
    /// `sysfs` is the root of sysfs, `/sys` outside the tests.
    fn plan(&mut self, sysfs: &CStr) -> Result<u32, Error>;
    /// Open the counters, disabled, and check that they count. On an
    /// error nothing stays open.
    fn open(&mut self) -> Result<(), Error>;
    /// Let the counters count.
    fn enable(&mut self) -> Result<(), Error>;
    /// Stop the counters.
    fn disable(&mut self) -> Result<(), Error>;
    /// Read the values counted so far.
    fn read(&mut self) -> Result<Counts, Error>;
}

/// The backend of this build.
#[cfg(all(target_os = "macos", not(feature = "none")))]
pub type Platform = macos::Rusage;
/// The backend of this build.
#[cfg(not(all(target_os = "macos", not(feature = "none"))))]
pub type Platform = none::Unsupported;

/// The counters of one process and the references to them.
pub struct Counters<B> {
    backend: B,
    open: bool,
    refs: u32,
}

impl<B: Backend> Counters<B> {
    pub const fn new(backend: B) -> Self {
        Counters {
            backend,
            open: false,
            refs: 0,
        }
    }

    /// Whether the counters are open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The references held now.
    pub fn refs(&self) -> u32 {
        self.refs
    }

    /// The backend, for the tests.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// How many file descriptors the opening needs: 0 once open.
    pub fn plan(&mut self, sysfs: &CStr) -> Result<u32, Error> {
        if self.open {
            return Ok(0);
        }
        self.backend.plan(sysfs)
    }

    /// Open the counters once in the process; later calls do nothing.
    pub fn open(&mut self) -> Result<(), Error> {
        if self.open {
            return Ok(());
        }
        self.backend.open()?;
        self.open = true;
        Ok(())
    }

    /// Take a reference and read the counters. The first reference
    /// enables them. On an error the reference is not taken.
    pub fn acquire(&mut self) -> Result<Counts, Error> {
        if !self.open {
            return Err(Error::new(Status::Internal));
        }
        let refs = self
            .refs
            .checked_add(1)
            .ok_or(Error::new(Status::Internal))?;
        if self.refs == 0 {
            self.backend.enable()?;
        }
        match self.backend.read() {
            Ok(counts) => {
                self.refs = refs;
                Ok(counts)
            }
            Err(error) => {
                if self.refs == 0 {
                    // The read already failed; that error is the one to
                    // report.
                    let _ = self.backend.disable();
                }
                Err(error)
            }
        }
    }

    /// Read the counters while a reference is held.
    pub fn read(&mut self) -> Result<Counts, Error> {
        if self.refs == 0 {
            return Err(Error::new(Status::Internal));
        }
        self.backend.read()
    }

    /// Give a reference back. The last one disables the counters; the
    /// reference is given back even when disabling fails.
    pub fn release(&mut self) -> Result<(), Error> {
        let refs = self
            .refs
            .checked_sub(1)
            .ok_or(Error::new(Status::Internal))?;
        self.refs = refs;
        if refs == 0 {
            self.backend.disable()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]

    use super::*;

    /// A backend that counts its calls and can be told to fail.
    #[derive(Default)]
    struct Fake {
        enabled: bool,
        enables: u32,
        disables: u32,
        fail_read: bool,
    }

    impl Backend for Fake {
        fn plan(&mut self, _: &CStr) -> Result<u32, Error> {
            Ok(2)
        }
        fn open(&mut self) -> Result<(), Error> {
            Ok(())
        }
        fn enable(&mut self) -> Result<(), Error> {
            self.enabled = true;
            self.enables += 1;
            Ok(())
        }
        fn disable(&mut self) -> Result<(), Error> {
            self.enabled = false;
            self.disables += 1;
            Ok(())
        }
        fn read(&mut self) -> Result<Counts, Error> {
            if self.fail_read {
                Err(Error::new(Status::CounterLost))
            } else {
                Ok(Counts::default())
            }
        }
    }

    #[test]
    fn first_reference_enables_and_last_disables() {
        let mut c = Counters::new(Fake::default());
        assert_eq!(c.plan(c"/sys").unwrap(), 2);
        c.open().unwrap();
        assert_eq!(c.plan(c"/sys").unwrap(), 0);
        c.acquire().unwrap();
        c.acquire().unwrap();
        assert_eq!(c.backend().enables, 1);
        c.release().unwrap();
        assert!(c.backend().enabled);
        c.release().unwrap();
        assert!(!c.backend().enabled);
        assert_eq!(c.backend().disables, 1);
    }

    #[test]
    fn steps_out_of_order_are_internal_errors() {
        let mut c = Counters::new(Fake::default());
        assert_eq!(c.acquire().unwrap_err().status, Status::Internal);
        c.open().unwrap();
        assert_eq!(c.read().unwrap_err().status, Status::Internal);
        assert_eq!(c.release().unwrap_err().status, Status::Internal);
    }

    #[test]
    fn failed_first_acquire_leaves_nothing_enabled() {
        let mut c = Counters::new(Fake {
            fail_read: true,
            ..Fake::default()
        });
        c.open().unwrap();
        assert_eq!(c.acquire().unwrap_err().status, Status::CounterLost);
        assert_eq!(c.refs(), 0);
        assert!(!c.backend().enabled);
    }
}
