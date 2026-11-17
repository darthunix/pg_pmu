//! The version that cannot count: built with the feature `none`, and
//! on every system other than Linux and macOS.

use core::ffi::CStr;

use super::{Backend, Set};
use crate::{Error, Status};

/// Refuses to open, so nothing else is ever called.
pub struct Unsupported;

impl Unsupported {
    pub const fn new() -> Self {
        Unsupported
    }
}

impl Default for Unsupported {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for Unsupported {
    fn plan(&mut self, _: &CStr, _: Set) -> Result<u32, Error> {
        Ok(0)
    }

    fn open(&mut self, _: Set) -> Result<(), Error> {
        Err(Error::new(Status::NoSystem))
    }

    fn enable(&mut self, _: Set) -> Result<(), Error> {
        Err(Error::new(Status::Internal))
    }

    fn disable(&mut self, _: Set) -> Result<(), Error> {
        Err(Error::new(Status::Internal))
    }

    fn read(&mut self, _: Set) -> Result<(u64, u64), Error> {
        Err(Error::new(Status::Internal))
    }
}
