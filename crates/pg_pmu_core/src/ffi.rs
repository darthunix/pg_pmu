//! The functions C calls, declared in `pg_pmu_core.h`.
//!
//! Each returns a [`Status`] as an `int32_t`, and writes the error of
//! the system to `os_error`, 0 when there is none. A null pointer is
//! an internal error, never a crash.

use core::cell::UnsafeCell;
use core::ffi::{CStr, c_char};
use core::mem::{offset_of, size_of};

use crate::counters::{Counters, Counts, Platform};
use crate::{Error, Status};

/// State of one process.
struct ProcessCell<T>(UnsafeCell<T>);

// SAFETY: a process of PostgreSQL calls the module, and so the crate,
// from its one main thread only, and never from a signal handler; no
// two calls run at once.
unsafe impl<T> Sync for ProcessCell<T> {}

static COUNTERS: ProcessCell<Counters<Platform>> =
    ProcessCell(UnsafeCell::new(Counters::new(Platform::new())));

fn counters() -> &'static mut Counters<Platform> {
    // SAFETY: see ProcessCell; every function below takes this
    // reference once and drops it before it returns.
    unsafe { &mut *COUNTERS.0.get() }
}

/// Write the error of the system, if the pointer is not null, and
/// return the code.
fn finish(result: Result<(), Error>, os_error: *mut i32) -> i32 {
    let (status, os) = match result {
        Ok(()) => (Status::Ok, 0),
        Err(e) => (e.status, e.os_error),
    };
    if !os_error.is_null() {
        // SAFETY: C passes the address of an int32_t or null.
        unsafe { os_error.write(os) };
    }
    status as i32
}

/// Store counts through a pointer C gave, or fail if it is null.
fn store(out: *mut Counts, counts: Counts) -> Result<(), Error> {
    if out.is_null() {
        return Err(Error::new(Status::Internal));
    }
    // SAFETY: C passes the address of a PmuCounts.
    unsafe { out.write(counts) };
    Ok(())
}

/// The sizes and offsets of the types C shares with the crate, so
/// that C can check its header against the crate when it loads.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PmuLayout {
    pub counts_size: u32,
    pub counts_cycles: u32,
}

/// Fill the layout of the shared types.
///
/// # Safety
///
/// `out` is the address of a `PmuLayout`, or null, which does nothing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_core_layout(out: *mut PmuLayout) {
    if out.is_null() {
        return;
    }
    let layout = PmuLayout {
        counts_size: size_of::<Counts>() as u32,
        counts_cycles: offset_of!(Counts, cycles) as u32,
    };
    // SAFETY: see above.
    unsafe { out.write(layout) };
}

/// Find what must be opened, and write how many file descriptors the
/// opening needs, from PostgreSQL's budget of external files; 0 once
/// the counters are open.
///
/// # Safety
///
/// `sysfs` is a string ending in zero; `descriptors` and `os_error`
/// are addresses of their types, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_counters_plan(
    sysfs: *const c_char,
    descriptors: *mut u32,
    os_error: *mut i32,
) -> i32 {
    let result = (|| {
        if sysfs.is_null() || descriptors.is_null() {
            return Err(Error::new(Status::Internal));
        }
        // SAFETY: C passes a string ending in zero.
        let sysfs = unsafe { CStr::from_ptr(sysfs) };
        let n = counters().plan(sysfs)?;
        // SAFETY: C passes the address of a uint32_t.
        unsafe { descriptors.write(n) };
        Ok(())
    })();
    finish(result, os_error)
}

/// Open the counters, once in the process. On an error nothing stays
/// open, and the caller gives back the descriptors it took.
///
/// # Safety
///
/// `os_error` is the address of an `int32_t`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_counters_open(os_error: *mut i32) -> i32 {
    finish(counters().open(), os_error)
}

/// Take a reference to the counters and read them.
///
/// # Safety
///
/// `now` is the address of a `PmuCounts`; `os_error` that of an
/// `int32_t`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_counters_acquire(now: *mut Counts, os_error: *mut i32) -> i32 {
    let result = (|| {
        if now.is_null() {
            return Err(Error::new(Status::Internal));
        }
        let counts = counters().acquire()?;
        store(now, counts)
    })();
    finish(result, os_error)
}

/// Read the counters while a reference is held.
///
/// # Safety
///
/// As for [`pmu_counters_acquire`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_counters_read(now: *mut Counts, os_error: *mut i32) -> i32 {
    let result = (|| {
        if now.is_null() {
            return Err(Error::new(Status::Internal));
        }
        let counts = counters().read()?;
        store(now, counts)
    })();
    finish(result, os_error)
}

/// Give a reference back.
///
/// # Safety
///
/// `os_error` is the address of an `int32_t`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_counters_release(os_error: *mut i32) -> i32 {
    finish(counters().release(), os_error)
}
