//! The parts of pg_pmu that are checked best in Rust: the counters of
//! the processor for one process, and, later, the cells of the shared
//! memory where the parallel workers report.
//!
//! The crate is linked into the C module as a static library. C calls
//! the functions in [`ffi`], declared by hand in `pg_pmu_core.h`, and
//! turns their codes ([`Status`]) into errors of PostgreSQL.
//!
//! The crate does not use the standard library, except in its tests,
//! and must not panic: a panic aborts the process, and PostgreSQL then
//! restarts every session.

#![cfg_attr(not(any(test, feature = "std", kani, loom)), no_std)]

#[cfg(not(target_has_atomic = "64"))]
compile_error!("pg_pmu needs a processor with native 64-bit atomics");

pub mod cells;
pub mod counters;
pub mod ffi;
mod status;
pub mod sysfs;

pub use status::{Error, Status};

/// The last resort, which nothing of the interface reaches: every
/// function returns a code instead.
#[cfg(not(any(test, feature = "std", kani, loom)))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    // SAFETY: abort has no preconditions.
    unsafe { libc::abort() }
}
