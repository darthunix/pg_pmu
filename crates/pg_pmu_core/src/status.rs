//! The codes the crate returns to C. Their numbers are part of the
//! interface: `pg_pmu_core.h` defines the same, and a test compares
//! them.

/// What went wrong, as C turns it into an error of PostgreSQL. The
/// SQLSTATE each one becomes is in the comment.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Done.
    Ok = 0,
    /// 42501: the system does not allow the process to count.
    NotPermitted = 1,
    /// 0A000: built without counters, or a system other than Linux and
    /// macOS.
    NoSystem = 2,
    /// 0A000: the kernel has no counters for the process, as in a
    /// virtual machine.
    NoCounters = 3,
    /// 0A000: the kinds of cores do not cover the processors online.
    CoresNotCovered = 4,
    /// 0A000: the kernel cannot open an event on one kind of cores.
    KindNotOpenable = 5,
    /// 53000: no hardware counter is free.
    NoFreeCounter = 6,
    /// 53000: no file descriptor is left.
    NoDescriptor = 7,
    /// 53000: the system took a counter away while it counted.
    CounterLost = 8,
    /// XX000: a call of the system failed in a way not foreseen.
    SystemError = 9,
    /// XX000: the module called the crate out of order.
    Internal = 10,
}

/// A code with the error of the system, `errno`, where there is one,
/// and 0 where there is none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub status: Status,
    pub os_error: i32,
}

impl Error {
    /// An error with no error of the system.
    pub const fn new(status: Status) -> Self {
        Error {
            status,
            os_error: 0,
        }
    }

    /// An error with the given error of the system.
    pub const fn os(status: Status, os_error: i32) -> Self {
        Error { status, os_error }
    }
}
