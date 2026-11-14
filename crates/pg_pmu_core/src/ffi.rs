//! The functions C calls, declared in `pg_pmu_core.h`.
//!
//! Each returns a [`Status`] as an `int32_t`, and writes the error of
//! the system to `os_error`, 0 when there is none. A null pointer is
//! an internal error, never a crash.

use core::cell::UnsafeCell;
use core::ffi::{CStr, c_char};
use core::mem::{offset_of, size_of};

use crate::counters::{Counters, Counts, Platform, platform};
use crate::{Error, Status};

/// The counters of this process.
struct ProcessCounters(UnsafeCell<Counters<Platform>>);

// SAFETY: a process of PostgreSQL calls the module, and so the crate,
// from its one main thread only, and never from a signal handler; no
// two calls run at once, and none calls back into another.
unsafe impl Sync for ProcessCounters {}

static COUNTERS: ProcessCounters = ProcessCounters(UnsafeCell::new(Counters::new(platform())));

/// Run `f` with the counters of this process. The reference lives only
/// as long as `f`, and `f` does not call back into this module.
fn with_counters<R>(f: impl FnOnce(&mut Counters<Platform>) -> R) -> R {
    // SAFETY: see ProcessCounters: no other reference to the counters
    // exists while f runs.
    f(unsafe { &mut *COUNTERS.0.get() })
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
    pub perf_abi_size: u32,
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
        perf_abi_size: size_of::<PmuPerfAbi>() as u32,
    };
    // SAFETY: see above.
    unsafe { out.write(layout) };
}

/// `PmuPerfAbi` of `pg_pmu_core.h`: what the crate puts in a
/// `perf_event_attr` and passes to the kernel, so that C can compare it
/// with `<linux/perf_event.h>` when the module loads on Linux.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PmuPerfAbi {
    pub attr_size: u32,
    pub config_offset: u32,
    pub read_format_offset: u32,
    pub flags_offset: u32,
    pub type_hardware: u32,
    pub pmu_type_shift: u32,
    pub hw_instructions: u64,
    pub hw_cycles: u64,
    pub format_group: u64,
    pub leader_flags: u64,
    pub member_flags: u64,
    pub flag_fd_cloexec: u64,
    pub ioc_enable: u64,
    pub ioc_disable: u64,
    pub ioc_flag_group: u64,
}

/// Fill what the crate uses of perf events.
///
/// # Safety
///
/// `out` is the address of a `PmuPerfAbi`, or null, which does nothing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_core_perf_abi(out: *mut PmuPerfAbi) {
    use crate::counters::perf_abi as abi;
    if out.is_null() {
        return;
    }
    let values = PmuPerfAbi {
        attr_size: abi::PERF_ATTR_SIZE_VER0,
        config_offset: offset_of!(abi::PerfEventAttr, config) as u32,
        read_format_offset: offset_of!(abi::PerfEventAttr, read_format) as u32,
        flags_offset: offset_of!(abi::PerfEventAttr, flags) as u32,
        type_hardware: abi::PERF_TYPE_HARDWARE,
        pmu_type_shift: abi::PERF_PMU_TYPE_SHIFT,
        hw_instructions: abi::PERF_COUNT_HW_INSTRUCTIONS,
        hw_cycles: abi::PERF_COUNT_HW_CPU_CYCLES,
        format_group: abi::PERF_FORMAT_GROUP,
        leader_flags: abi::LEADER_FLAGS,
        member_flags: abi::MEMBER_FLAGS,
        flag_fd_cloexec: abi::PERF_FLAG_FD_CLOEXEC,
        ioc_enable: abi::PERF_EVENT_IOC_ENABLE,
        ioc_disable: abi::PERF_EVENT_IOC_DISABLE,
        ioc_flag_group: abi::PERF_IOC_FLAG_GROUP,
    };
    // SAFETY: see above.
    unsafe { out.write(values) };
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
        let n = with_counters(|c| c.plan(sysfs))?;
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
    finish(with_counters(|c| c.open()), os_error)
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
        let counts = with_counters(|c| c.acquire())?;
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
        let counts = with_counters(|c| c.read())?;
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
    finish(with_counters(|c| c.release()), os_error)
}
