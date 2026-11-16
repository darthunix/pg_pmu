//! The counters of Linux: for each thread of the process and each kind
//! of cores, one group of two perf events, instructions and cycles, on
//! that thread and any CPU, in user space only.
//!
//! The threads are those that run when the counters open, listed once.
//! Every event is inherited: a thread or a process that a counted
//! thread starts later gets copies of its groups, and a read of a group
//! adds the copies in, those of threads that have ended too. So the
//! groups count the whole process, and no thread twice: listing the
//! threads again would give those started later a second group. Linux
//! before 4.13 refuses inheritance with reads of a group; there the
//! events are not inherited, and threads started later are not counted.
//!
//! Each group is pinned: the kernel keeps it on hardware counters all
//! the time, or puts it into an error state where a read returns no
//! bytes, so counts are never scaled from a part of the time. It is
//! opened disabled and enabled while a reference is held; enabling it
//! again takes it out of an error state. The calls of the system go
//! through [`PerfSys`], so that all of this is tested on any system.

use core::ffi::CStr;

use super::perf_abi::{
    self, FLAG_INHERIT, PERF_COUNT_HW_CPU_CYCLES, PERF_COUNT_HW_INSTRUCTIONS,
    PERF_EVENT_IOC_DISABLE, PERF_EVENT_IOC_ENABLE, PERF_IOC_FLAG_GROUP, PerfEventAttr,
};
use super::{Backend, Counts};
use crate::sysfs::{self, LibcFs, MAX_KINDS, Plan};
use crate::{Error, Status};

/// The calls of the system the counters make. Errors are `errno`.
pub trait PerfSys {
    /// `perf_event_open` for the thread `tid` on any CPU; return the
    /// file descriptor.
    fn open(&mut self, attr: &PerfEventAttr, tid: i32, group_fd: i32) -> Result<i32, i32>;
    fn ioctl(&mut self, fd: i32, request: u64, arg: u64) -> Result<(), i32>;
    /// `read`; return the number of bytes read.
    fn read(&mut self, fd: i32, buf: &mut [u8]) -> Result<usize, i32>;
    fn close(&mut self, fd: i32);
    /// The ids of the threads of this process: write the first ones to
    /// `out`, and return how many there are, which may be more.
    fn threads(&mut self, out: &mut [i32]) -> Result<usize, i32>;
    /// What the processor says of its kinds of cores, where it says.
    fn uniform_cores(&self) -> Option<bool>;
}

/// The two events of a group, each a file descriptor; -1 when closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Group {
    leader: i32,
    member: i32,
}

const CLOSED: Group = Group {
    leader: -1,
    member: -1,
};

/// Bytes of a read of a group: the number of events, then their values.
const READ_LEN: usize = 8 * 3;

/// The threads that may run when the counters open. A process of
/// PostgreSQL has one; more than this are refused rather than counted
/// in part.
pub const MAX_THREADS: usize = 64;

/// The id of a thread from the name of its directory in
/// `/proc/self/task`: decimal digits, from 1 to `i32::MAX`. `None` for
/// any other name, such as `.` and `..`.
pub fn parse_tid(name: &[u8]) -> Option<i32> {
    if name.is_empty() {
        return None;
    }
    let mut tid: i32 = 0;
    for &b in name {
        if !b.is_ascii_digit() {
            return None;
        }
        tid = tid
            .checked_mul(10)?
            .checked_add(i32::from(b.wrapping_sub(b'0')))?;
    }
    (tid > 0).then_some(tid)
}

/// The values of a read of a group of two events, or `None` if the read
/// is not exactly that: of the right length, saying two events.
pub fn parse_group_read(buf: &[u8]) -> Option<(u64, u64)> {
    if buf.len() != READ_LEN {
        return None;
    }
    let word = |i: usize| -> Option<u64> {
        let start = i.checked_mul(8)?;
        let bytes: [u8; 8] = buf.get(start..start.checked_add(8)?)?.try_into().ok()?;
        Some(u64::from_ne_bytes(bytes))
    };
    if word(0)? != 2 {
        return None;
    }
    Some((word(1)?, word(2)?))
}

/// What an error of `perf_event_open` means. `typed` is whether the
/// event named a kind of cores.
pub fn open_error(errno: i32, typed: bool) -> Error {
    let status = match errno {
        libc::EACCES | libc::EPERM => Status::NotPermitted,
        // ENOSYS: no perf events at all, as under gVisor or some filters
        // of system calls.
        libc::ENOSYS => Status::NoCounters,
        libc::ENOENT | libc::EOPNOTSUPP | libc::ENODEV if typed => Status::KindNotOpenable,
        libc::ENOENT | libc::EOPNOTSUPP | libc::ENODEV => Status::NoCounters,
        libc::EINVAL if typed => Status::KindNotOpenable,
        libc::EMFILE | libc::ENFILE => Status::NoDescriptor,
        libc::EBUSY => Status::NoFreeCounter,
        _ => Status::SystemError,
    };
    Error::os(status, errno)
}

pub struct Perf<S> {
    sys: S,
    plan: Option<Plan>,
    /// The threads the plan listed, of which the first `nthreads`.
    threads: [i32; MAX_THREADS],
    nthreads: usize,
    /// The groups of each thread, one for each kind of cores; closed for
    /// a thread that ended before the opening.
    groups: [[Group; MAX_KINDS]; MAX_THREADS],
    /// Whether the events are inherited: false once the kernel refused.
    inherit: bool,
}

impl<S: PerfSys> Perf<S> {
    pub const fn with(sys: S) -> Self {
        Perf {
            sys,
            plan: None,
            threads: [0; MAX_THREADS],
            nthreads: 0,
            groups: [[CLOSED; MAX_KINDS]; MAX_THREADS],
            inherit: true,
        }
    }

    /// Whether the events are inherited by threads started later.
    pub fn inherits(&self) -> bool {
        self.inherit
    }

    /// The system, for the tests.
    pub fn sys(&self) -> &S {
        &self.sys
    }

    /// The system, for the tests.
    pub fn sys_mut(&mut self) -> &mut S {
        &mut self.sys
    }

    /// Take a plan of the kinds of cores made elsewhere, as the tests do,
    /// and list the threads; return the number of descriptors the
    /// opening needs at most: a thread that ends before it uses none.
    pub fn set_plan(&mut self, plan: Plan) -> Result<u32, Error> {
        let mut threads = [0; MAX_THREADS];
        let n = self.sys.threads(&mut threads).map_err(|e| match e {
            libc::EMFILE | libc::ENFILE => Error::os(Status::NoDescriptor, e),
            _ => Error::os(Status::SystemError, e),
        })?;
        if n > MAX_THREADS {
            return Err(Error::new(Status::TooManyThreads));
        }
        // The calling thread is always one of them.
        if n == 0 {
            return Err(Error::new(Status::SystemError));
        }
        self.threads = threads;
        self.nthreads = n;
        self.plan = Some(plan);
        Ok(descriptors(&plan, n))
    }

    fn count(&self) -> usize {
        self.plan.map_or(0, |p| p.count.min(MAX_KINDS))
    }

    /// Open an event on the thread `tid`, inherited unless the kernel
    /// refused. A kernel that refuses inheritance with reads of a group
    /// says EINVAL to the first leader; another EINVAL without it is an
    /// error of its own. The error is `errno`.
    fn open_event(
        &mut self,
        event: u64,
        unit: Option<u32>,
        tid: i32,
        group_fd: i32,
    ) -> Result<i32, i32> {
        let leader = group_fd == -1;
        let mut attr = perf_abi::attr(event, unit, leader);
        if !self.inherit {
            attr.flags &= !FLAG_INHERIT;
        }
        match self.sys.open(&attr, tid, group_fd) {
            Err(libc::EINVAL)
                if self.inherit && leader && self.groups.iter().flatten().all(|g| g.leader < 0) =>
            {
                attr.flags &= !FLAG_INHERIT;
                let fd = self.sys.open(&attr, tid, group_fd)?;
                self.inherit = false;
                Ok(fd)
            }
            result => result,
        }
    }

    /// Open a group on the thread `tid`; the error is `errno`.
    fn open_group(&mut self, unit: Option<u32>, tid: i32) -> Result<Group, i32> {
        let leader = self.open_event(PERF_COUNT_HW_INSTRUCTIONS, unit, tid, -1)?;
        match self.open_event(PERF_COUNT_HW_CPU_CYCLES, unit, tid, leader) {
            Ok(member) => Ok(Group { leader, member }),
            Err(e) => {
                self.sys.close(leader);
                Err(e)
            }
        }
    }

    /// Close the groups of the thread at `index` in the list.
    fn close_thread(&mut self, index: usize) {
        if let Some(row) = self.groups.get_mut(index) {
            for g in row.iter_mut() {
                if g.member >= 0 {
                    self.sys.close(g.member);
                }
                if g.leader >= 0 {
                    self.sys.close(g.leader);
                }
                *g = CLOSED;
            }
        }
    }

    fn close_all(&mut self) {
        for index in 0..MAX_THREADS {
            self.close_thread(index);
        }
    }

    /// Open the groups of the thread at `index` in the list, one for
    /// each kind. A thread that ended after the listing has nothing left
    /// to count, and keeps no group. Its id could in theory go to a new
    /// thread of another process of the user in between; that needs the
    /// ids to wrap around within the opening.
    fn open_thread(&mut self, plan: &Plan, index: usize) -> Result<(), Error> {
        let tid = self
            .threads
            .get(index)
            .copied()
            .ok_or(Error::new(Status::Internal))?;
        for kind in 0..self.count() {
            let unit = if plan.typed {
                plan.types.get(kind).copied()
            } else {
                None
            };
            let g = match self.open_group(unit, tid) {
                Ok(g) => g,
                Err(libc::ESRCH) => {
                    self.close_thread(index);
                    return Ok(());
                }
                Err(e) => return Err(open_error(e, unit.is_some())),
            };
            match self.groups.get_mut(index).and_then(|row| row.get_mut(kind)) {
                Some(slot) => *slot = g,
                None => {
                    self.sys.close(g.member);
                    self.sys.close(g.leader);
                    return Err(Error::new(Status::Internal));
                }
            }
        }
        Ok(())
    }

    fn each_leader(&mut self, request: u64) -> Result<(), Error> {
        let kinds = self.count();
        for row in self.groups.iter().take(self.nthreads) {
            for g in row.iter().take(kinds).filter(|g| g.leader >= 0) {
                self.sys
                    .ioctl(g.leader, request, PERF_IOC_FLAG_GROUP)
                    .map_err(|e| Error::os(Status::SystemError, e))?;
            }
        }
        Ok(())
    }

    /// Read and add the groups; `lost` is the code of a group in the
    /// error state.
    fn read_groups(&mut self, lost: Status) -> Result<Counts, Error> {
        let mut total = Counts::default();
        let kinds = self.count();
        for g in self
            .groups
            .iter()
            .take(self.nthreads)
            .flat_map(|row| row.iter().take(kinds))
            .filter(|g| g.leader >= 0)
        {
            let mut buf = [0u8; READ_LEN + 8];
            let n = self
                .sys
                .read(g.leader, &mut buf)
                .map_err(|e| Error::os(Status::SystemError, e))?;
            // No bytes: the pinned group is in the error state. Any other
            // length, or another number of events, is never reused.
            let (instructions, cycles) = buf
                .get(..n)
                .and_then(parse_group_read)
                .ok_or(Error::new(lost))?;
            total.instructions = total.instructions.wrapping_add(instructions);
            total.cycles = total.cycles.wrapping_add(cycles);
        }
        Ok(total)
    }
}

/// Two descriptors for each group, a group for each kind and thread.
fn descriptors(plan: &Plan, threads: usize) -> u32 {
    let kinds = u32::try_from(plan.count.min(MAX_KINDS)).unwrap_or(0);
    let threads = u32::try_from(threads.min(MAX_THREADS)).unwrap_or(0);
    kinds.saturating_mul(threads).saturating_mul(2)
}

impl<S: PerfSys> Backend for Perf<S> {
    fn plan(&mut self, sysfs: &CStr) -> Result<u32, Error> {
        let fs = LibcFs {
            root: sysfs.to_bytes(),
        };
        let plan = sysfs::discover(&fs, self.sys.uniform_cores()).map_err(Error::new)?;
        self.set_plan(plan)
    }

    fn open(&mut self) -> Result<(), Error> {
        let plan = self.plan.ok_or(Error::new(Status::Internal))?;
        for index in 0..self.nthreads {
            if let Err(e) = self.open_thread(&plan, index) {
                self.close_all();
                return Err(e);
            }
        }
        // Check that the groups count: a group that finds no free
        // counter is in the error state and reads no bytes, and a group
        // that the hypervisor opens but never counts reads zero.
        let check = self
            .each_leader(PERF_EVENT_IOC_ENABLE)
            .and_then(|()| self.read_groups(Status::NoFreeCounter))
            .and_then(|counts| {
                if counts.instructions == 0 {
                    Err(Error::new(Status::NoCounters))
                } else {
                    Ok(())
                }
            })
            .and_then(|()| self.each_leader(PERF_EVENT_IOC_DISABLE));
        if let Err(e) = check {
            let _ = self.each_leader(PERF_EVENT_IOC_DISABLE);
            self.close_all();
            return Err(e);
        }
        Ok(())
    }

    fn enable(&mut self) -> Result<(), Error> {
        // A failure midway leaves no group on.
        self.each_leader(PERF_EVENT_IOC_ENABLE).inspect_err(|_| {
            let _ = self.each_leader(PERF_EVENT_IOC_DISABLE);
        })
    }

    fn disable(&mut self) -> Result<(), Error> {
        self.each_leader(PERF_EVENT_IOC_DISABLE)
    }

    fn read(&mut self) -> Result<Counts, Error> {
        self.read_groups(Status::CounterLost)
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// The check of a read accepts exactly a read of three words saying
    /// two events, for every buffer of up to 64 bytes.
    #[kani::proof]
    #[kani::unwind(70)]
    fn group_read_is_checked() {
        let buf: [u8; 64] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= 64);
        let part = &buf[..len];
        let word = |i: usize| u64::from_ne_bytes(buf[i * 8..i * 8 + 8].try_into().unwrap());
        let nr = if len >= 8 { word(0) } else { 0 };
        match parse_group_read(part) {
            Some((instructions, cycles)) => {
                assert!(len == READ_LEN && nr == 2);
                assert_eq!((instructions, cycles), (word(1), word(2)));
            }
            None => assert!(len != READ_LEN || nr != 2),
        }
    }
}
