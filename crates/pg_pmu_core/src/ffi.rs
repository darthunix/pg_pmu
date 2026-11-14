//! The functions C calls, declared in `pg_pmu_core.h`.
//!
//! Each returns a [`Status`] as an `int32_t`, and writes the error of
//! the system to `os_error`, 0 when there is none. A null pointer is
//! an internal error, never a crash.

use core::cell::UnsafeCell;
use core::ffi::{CStr, c_char};
use core::mem::{offset_of, size_of};

use crate::cells::{self, Cell, Contents, Header, LOW_MASK};
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
    pub contents_size: u32,
    pub contents_failures: u32,
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
        contents_size: size_of::<Contents>() as u32,
        contents_failures: offset_of!(Contents, failures) as u32,
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

// ---------------------------------------------------------------------
// The cells of the workers
// ---------------------------------------------------------------------
//
// C allocates the memory "pg_pmu" and passes its address to each call;
// the crate reads the numbers of cells from its header. The memory
// lives as long as the server, and every process maps it.

/// The code of a step on the cells.
fn code(result: Result<(), Status>) -> i32 {
    match result {
        Ok(()) => Status::Ok as i32,
        Err(s) => s as i32,
    }
}

/// The cells of the backends and of the pool at `base`.
///
/// # Safety
///
/// `base` is null, or the address `pmu_cells_init` set up, which lives
/// as long as the process.
unsafe fn cells_at<'a>(base: *const u8) -> Result<(&'a [Cell], usize), Status> {
    if base.is_null() {
        return Err(Status::Internal);
    }
    // SAFETY: the header was written once, before any process used the
    // memory, and is only read since.
    let header = unsafe { &*base.cast::<Header>() };
    let n = usize::try_from(header.n).map_err(|_| Status::Internal)?;
    let k = usize::try_from(header.k).map_err(|_| Status::Internal)?;
    let count = n.checked_add(k).ok_or(Status::Internal)?;
    // SAFETY: the cells follow the header, `count` of them, aligned,
    // and are atomic, so shared references to them may be used by
    // every process at once.
    let cells =
        unsafe { core::slice::from_raw_parts(base.add(size_of::<Header>()).cast::<Cell>(), count) };
    Ok((cells, n))
}

/// Take a cell with `take`, which gives its index and generation, and
/// write its token. The pointer is checked first, so that no cell is
/// taken whose token would be lost.
fn take_into(out: *mut u64, take: impl FnOnce() -> Result<(u32, u64), Status>) -> i32 {
    if out.is_null() {
        return Status::Internal as i32;
    }
    code(take().map(|(index, g)| {
        // SAFETY: C passes the address of a uint64_t.
        unsafe { out.write(cells::token(index, g)) }
    }))
}

/// The bytes of the memory for `n` cells of the backends and `k` of the
/// pool, or 0 when their numbers do not fit in a token: N + K + 1 must
/// fit in 20 bits.
#[unsafe(no_mangle)]
pub extern "C" fn pmu_cells_size(n: u32, k: u32) -> usize {
    match u64::from(n).checked_add(u64::from(k)) {
        Some(count) if count < LOW_MASK => cells::size(n, k).unwrap_or(0),
        _ => 0,
    }
}

/// Set up the memory: write the header and free cells.
///
/// # Safety
///
/// `base` is the address of `pmu_cells_size(n, k)` bytes, aligned to 8,
/// that no process uses yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_cells_init(base: *mut u8, n: u32, k: u32) -> i32 {
    let result = (|| {
        if base.is_null()
            || pmu_cells_size(n, k) == 0
            || base.align_offset(core::mem::align_of::<Cell>()) != 0
        {
            return Err(Status::Internal);
        }
        // SAFETY: see above; nothing else reads the memory yet.
        unsafe {
            base.cast::<Header>().write(Header { n, k, zero: 0 });
            let first = base.add(size_of::<Header>()).cast::<Cell>();
            for i in 0..u64::from(n).wrapping_add(u64::from(k)) {
                let i = usize::try_from(i).map_err(|_| Status::Internal)?;
                first.add(i).write(Cell::default());
            }
        }
        Ok(())
    })();
    code(result)
}

/// Take the cell of backend `index`, from 0 to N - 1, and write its
/// token. A cell that is not free is an internal error: only its
/// backend takes it.
///
/// # Safety
///
/// `base` as for `pmu_cells_init`, set up; `token` the address of a
/// `uint64_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_cell_take(base: *const u8, index: u32, token: *mut u64) -> i32 {
    take_into(token, || {
        // SAFETY: see above.
        let (cells, n) = unsafe { cells_at(base) }?;
        let i = usize::try_from(index).map_err(|_| Status::Internal)?;
        if i >= n {
            return Err(Status::Internal);
        }
        let g = cells.get(i).and_then(Cell::take).ok_or(Status::Internal)?;
        Ok((index, g))
    })
}

/// Take a free cell of the pool and write its token; `PMU_NO_CELL`
/// when none is free.
///
/// # Safety
///
/// As for `pmu_cell_take`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_cell_take_pool(base: *const u8, token: *mut u64) -> i32 {
    take_into(token, || {
        // SAFETY: see above.
        let (cells, n) = unsafe { cells_at(base) }?;
        let pool = cells.get(n..).ok_or(Status::Internal)?;
        let (i, g) = cells::take_from_pool(pool).ok_or(Status::NoCell)?;
        // The index fits: N + K is below 2^20, as the size checked.
        let index = n.checked_add(i).and_then(|i| u32::try_from(i).ok());
        index.map(|index| (index, g)).ok_or(Status::Internal)
    })
}

/// Give back the cell of `token` and write what it held. Refused with
/// `PMU_BAD_TOKEN` for a token that is not valid, `PMU_STALE_TOKEN`
/// when the cell was given back already, and `PMU_CELL_BUSY` while a
/// worker adds to it; a refused return changes nothing.
///
/// # Safety
///
/// `base` as for `pmu_cell_take`; `out` the address of a
/// `PmuCellContents`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_cell_give_back(
    base: *const u8,
    token: u64,
    out: *mut Contents,
) -> i32 {
    let result = (|| {
        if out.is_null() {
            return Err(Status::Internal);
        }
        // SAFETY: see above.
        let (cell, g) = unsafe { cell_of(base, token) }?;
        let contents = cell.give_back(g)?;
        // SAFETY: C passes the address of a PmuCellContents.
        unsafe { out.write(contents) };
        Ok(())
    })();
    code(result)
}

/// The cell and the generation of `token`, or `PMU_BAD_TOKEN` for a
/// token that is not valid for this memory.
///
/// # Safety
///
/// As for `cells_at`.
unsafe fn cell_of<'a>(base: *const u8, token: u64) -> Result<(&'a Cell, u64), Status> {
    // SAFETY: see above.
    let (cells, _) = unsafe { cells_at(base) }?;
    let count = u32::try_from(cells.len()).map_err(|_| Status::Internal)?;
    let (i, g) = cells::split(token, count).ok_or(Status::BadToken)?;
    Ok((cells.get(i).ok_or(Status::BadToken)?, g))
}

/// Read what the cell of `token` holds, refused as for a return.
///
/// # Safety
///
/// As for `pmu_cell_give_back`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_cell_read(base: *const u8, token: u64, out: *mut Contents) -> i32 {
    let result = (|| {
        if out.is_null() {
            return Err(Status::Internal);
        }
        // SAFETY: see above.
        let (cell, g) = unsafe { cell_of(base, token) }?;
        let contents = cell.read(g)?;
        // SAFETY: C passes the address of a PmuCellContents.
        unsafe { out.write(contents) };
        Ok(())
    })();
    code(result)
}

/// Read a token from its decimal form, the value of `pg_pmu.cell`: 0
/// for no cell, else a token valid for this memory, or
/// `PMU_BAD_TOKEN`.
///
/// # Safety
///
/// `base` as for `pmu_cell_take`; `text` a string ending in zero;
/// `token` the address of a `uint64_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_token_parse(
    base: *const u8,
    text: *const c_char,
    token: *mut u64,
) -> i32 {
    let result = (|| {
        if text.is_null() || token.is_null() {
            return Err(Status::Internal);
        }
        // SAFETY: C passes a string ending in zero.
        let text = unsafe { CStr::from_ptr(text) };
        let t = cells::parse_token(text.to_bytes()).map_err(|_| Status::BadToken)?;
        if t != 0 {
            // SAFETY: see above.
            unsafe { cell_of(base, t) }?;
        }
        // SAFETY: C passes the address of a uint64_t.
        unsafe { token.write(t) };
        Ok(())
    })();
    code(result)
}

/// A worker's report: enter the cell of `token`, add the counts, leave.
/// Refused, adding nothing, for a token that is not valid
/// (`PMU_BAD_TOKEN`) or whose cell was given back (`PMU_STALE_TOKEN`).
///
/// # Safety
///
/// `base` as for `pmu_cell_take`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_cell_report(
    base: *const u8,
    token: u64,
    instructions: u64,
    cycles: u64,
) -> i32 {
    // SAFETY: see above.
    code(unsafe { cell_of(base, token) }.and_then(|(cell, g)| {
        cell.enter(g)?;
        cell.add(instructions, cycles);
        cell.leave()
    }))
}

/// The report of a worker that could not count, with the SQLSTATE of
/// its error as PostgreSQL packs it; refused as `pmu_cell_report`.
///
/// # Safety
///
/// As for `pmu_cell_report`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pmu_cell_report_failure(
    base: *const u8,
    token: u64,
    sqlstate: u32,
) -> i32 {
    // SAFETY: see above.
    code(unsafe { cell_of(base, token) }.and_then(|(cell, g)| {
        cell.enter(g)?;
        cell.fail(sqlstate);
        cell.leave()
    }))
}
