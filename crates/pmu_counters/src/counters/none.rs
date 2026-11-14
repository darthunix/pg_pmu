//! The version that cannot count: built with the feature `none`, and
//! on every system other than Linux and macOS.

use core::ffi::CStr;

use super::{Backend, Counts};
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
    fn plan(&mut self, _: &CStr) -> Result<u32, Error> {
        Ok(0)
    }

    fn open(&mut self) -> Result<(), Error> {
        Err(Error::new(Status::NoSystem))
    }

    fn enable(&mut self) -> Result<(), Error> {
        Err(Error::new(Status::Internal))
    }

    fn disable(&mut self) -> Result<(), Error> {
        Err(Error::new(Status::Internal))
    }

    fn read(&mut self) -> Result<Counts, Error> {
        Err(Error::new(Status::Internal))
    }
}
