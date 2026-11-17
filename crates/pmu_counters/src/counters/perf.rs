//! The counters of Linux: for each set of events, each thread of the
//! process and each kind of cores, one group of the two perf events of
//! the set, on that thread and any CPU, in user space only. The basic
//! set counts instructions and cycles; the others, opened only when a
//! statement asks for them, the branches and the cache with the
//! kernel's generic events.
//!
//! The threads of a set are those that run when it opens, listed once.
//! Every event is inherited: a thread or a process that a counted
//! thread starts later gets copies of its groups, and a read of a group
//! adds the copies in, those of threads that have ended too. So the
//! groups of a set count the whole process, and no thread twice:
//! listing the threads again would give those started later a second
//! group. Linux before 4.13 refuses inheritance with reads of a group;
//! there the events are not inherited, and threads started later are
//! not counted.
//!
//! Each group is pinned: the kernel keeps it on hardware counters all
//! the time, or puts it into an error state where a read returns no
//! bytes, so counts are never scaled from a part of the time. It is
//! opened disabled and enabled while a reference to its set is held;
//! enabling it again takes it out of an error state. The basic set is
//! opened first and the others when a statement first asks for them,
//! and the kernel puts the pinned groups of a thread on the counters in
//! the order they were made, so another set cannot take the counters of
//! the basic one; of two others, the one opened first wins. The calls
//! of the system go through [`PerfSys`], so that all of this is tested
//! on any system.

use core::ffi::CStr;

use super::perf_abi::{
    self, FLAG_INHERIT, PERF_COUNT_HW_BRANCH_INSTRUCTIONS, PERF_COUNT_HW_BRANCH_MISSES,
    PERF_COUNT_HW_CACHE_MISSES, PERF_COUNT_HW_CACHE_REFERENCES, PERF_COUNT_HW_CPU_CYCLES,
    PERF_COUNT_HW_INSTRUCTIONS, PERF_EVENT_IOC_DISABLE, PERF_EVENT_IOC_ENABLE, PERF_IOC_FLAG_GROUP,
    PerfEventAttr,
};
use super::{Backend, SETS, Set};
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
    /// Refer to the cache from the calling thread, so that a group of the
    /// cache that counts reads more than zero at the check of its
    /// opening: [`refer_to_the_cache`].
    fn refer_to_the_cache(&mut self) {
        refer_to_the_cache();
    }
}

/// The two events of a set: the leader of each group and its member.
pub const fn events_of(set: Set) -> (u64, u64) {
    match set {
        Set::Basic => (PERF_COUNT_HW_INSTRUCTIONS, PERF_COUNT_HW_CPU_CYCLES),
        Set::Branches => (
            PERF_COUNT_HW_BRANCH_INSTRUCTIONS,
            PERF_COUNT_HW_BRANCH_MISSES,
        ),
        Set::Cache => (PERF_COUNT_HW_CACHE_REFERENCES, PERF_COUNT_HW_CACHE_MISSES),
    }
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

/// The threads that may run when the counters of a set open. A process
/// of PostgreSQL has one; more than this are refused rather than
/// counted in part.
pub const MAX_THREADS: usize = 64;

/// The groups of one set: the threads listed when it was planned, of
/// which the first `nthreads`, and their groups, one for each kind of
/// cores; closed for a thread that ended before the opening.
#[derive(Clone, Copy)]
struct Groups {
    threads: [i32; MAX_THREADS],
    nthreads: usize,
    groups: [[Group; MAX_KINDS]; MAX_THREADS],
}

const NO_GROUPS: Groups = Groups {
    threads: [0; MAX_THREADS],
    nthreads: 0,
    groups: [[CLOSED; MAX_KINDS]; MAX_THREADS],
};

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

/// What an error of `perf_event_open` means for an event of the basic
/// set. `typed` is whether the event named a kind of cores.
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

/// What an error of `perf_event_open` means for an event of `set`. The
/// events of the other sets open only after the basic ones, with the
/// same attributes but the event, so the kernel takes the process, the
/// kinds of cores and the attributes: an event the processor or the
/// kernel lacks is what is left for ENOENT, EOPNOTSUPP, ENODEV and
/// EINVAL, which the kernel gives for one it does not map.
pub fn open_error_of(set: Set, errno: i32, typed: bool) -> Error {
    match (set, errno) {
        (Set::Basic, _) => open_error(errno, typed),
        (_, libc::ENOENT | libc::EOPNOTSUPP | libc::ENODEV | libc::EINVAL) => {
            Error::os(Status::EventsNotCounted, errno)
        }
        _ => open_error(errno, typed),
    }
}

pub struct Perf<S> {
    sys: S,
    /// The kinds of cores, planned with the basic set.
    plan: Option<Plan>,
    /// The groups of each set, in the order of [`Set::ALL`].
    sets: [Groups; SETS],
    /// Whether the events are inherited: false once the kernel refused.
    inherit: bool,
}

impl<S: PerfSys> Perf<S> {
    pub const fn with(sys: S) -> Self {
        Perf {
            sys,
            plan: None,
            sets: [NO_GROUPS; SETS],
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
    /// and plan the basic set; return the number of descriptors.
    pub fn set_plan(&mut self, plan: Plan) -> Result<u32, Error> {
        let n = self.list(Set::Basic)?;
        self.plan = Some(plan);
        Ok(descriptors(&plan, n))
    }

    /// Plan a set other than the basic one, on the kinds of cores of the
    /// basic one; return the number of descriptors.
    pub fn plan_set(&mut self, set: Set) -> Result<u32, Error> {
        let plan = self.plan.ok_or(Error::new(Status::Internal))?;
        if set == Set::Basic {
            return self.set_plan(plan);
        }
        let n = self.list(set)?;
        Ok(descriptors(&plan, n))
    }

    /// List the threads for a set; return how many. The opening needs
    /// descriptors for that many at most: a thread that ends before it
    /// uses none.
    fn list(&mut self, set: Set) -> Result<usize, Error> {
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
        let groups = self.groups_mut(set)?;
        groups.threads = threads;
        groups.nthreads = n;
        Ok(n)
    }

    fn groups(&self, set: Set) -> Option<&Groups> {
        self.sets.get(set.index())
    }

    fn groups_mut(&mut self, set: Set) -> Result<&mut Groups, Error> {
        self.sets
            .get_mut(set.index())
            .ok_or(Error::new(Status::Internal))
    }

    fn nthreads(&self, set: Set) -> usize {
        self.groups(set).map_or(0, |g| g.nthreads.min(MAX_THREADS))
    }

    fn count(&self) -> usize {
        self.plan.map_or(0, |p| p.count.min(MAX_KINDS))
    }

    /// The open groups of a set.
    fn open_groups(&self, set: Set) -> impl Iterator<Item = Group> + '_ {
        let kinds = self.count();
        self.groups(set)
            .into_iter()
            .flat_map(move |g| {
                g.groups
                    .iter()
                    .take(g.nthreads.min(MAX_THREADS))
                    .flat_map(move |row| row.iter().take(kinds))
            })
            .copied()
            .filter(|g| g.leader >= 0)
    }

    /// Open an event on the thread `tid`, inherited unless the kernel
    /// refused. A kernel that refuses inheritance with reads of a group
    /// says EINVAL to the first leader of the process; another EINVAL
    /// without it is an error of its own. The error is `errno`.
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
        let first = leader
            && Set::ALL
                .iter()
                .all(|&s| self.open_groups(s).next().is_none());
        match self.sys.open(&attr, tid, group_fd) {
            Err(libc::EINVAL) if self.inherit && first => {
                attr.flags &= !FLAG_INHERIT;
                let fd = self.sys.open(&attr, tid, group_fd)?;
                self.inherit = false;
                Ok(fd)
            }
            result => result,
        }
    }

    /// Open a group of a set on the thread `tid`; the error is `errno`.
    fn open_group(&mut self, set: Set, unit: Option<u32>, tid: i32) -> Result<Group, i32> {
        let (first, second) = events_of(set);
        let leader = self.open_event(first, unit, tid, -1)?;
        match self.open_event(second, unit, tid, leader) {
            Ok(member) => Ok(Group { leader, member }),
            Err(e) => {
                self.sys.close(leader);
                Err(e)
            }
        }
    }

    /// Close the groups of the thread at `index` in the list of a set.
    fn close_thread(&mut self, set: Set, index: usize) {
        let Some(row) = self
            .sets
            .get_mut(set.index())
            .and_then(|g| g.groups.get_mut(index))
        else {
            return;
        };
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

    fn close_set(&mut self, set: Set) {
        for index in 0..MAX_THREADS {
            self.close_thread(set, index);
        }
    }

    /// Open the groups of a set on the thread at `index` in its list,
    /// one for each kind. A thread that ended after the listing has
    /// nothing left to count, and keeps no group. Its id could in theory
    /// go to a new thread of another process of the user in between;
    /// that needs the ids to wrap around within the opening.
    fn open_thread(&mut self, set: Set, plan: &Plan, index: usize) -> Result<(), Error> {
        let tid = self
            .groups(set)
            .and_then(|g| g.threads.get(index))
            .copied()
            .ok_or(Error::new(Status::Internal))?;
        for kind in 0..self.count() {
            let unit = if plan.typed {
                plan.types.get(kind).copied()
            } else {
                None
            };
            let g = match self.open_group(set, unit, tid) {
                Ok(g) => g,
                Err(libc::ESRCH) => {
                    self.close_thread(set, index);
                    return Ok(());
                }
                Err(e) => return Err(open_error_of(set, e, unit.is_some())),
            };
            match self
                .sets
                .get_mut(set.index())
                .and_then(|groups| groups.groups.get_mut(index))
                .and_then(|row| row.get_mut(kind))
            {
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

    fn each_leader(&mut self, set: Set, request: u64) -> Result<(), Error> {
        let mut leaders = [-1; MAX_THREADS * MAX_KINDS];
        for (slot, g) in leaders.iter_mut().zip(self.open_groups(set)) {
            *slot = g.leader;
        }
        for &fd in leaders.iter().filter(|&&fd| fd >= 0) {
            self.sys
                .ioctl(fd, request, PERF_IOC_FLAG_GROUP)
                .map_err(|e| Error::os(Status::SystemError, e))?;
        }
        Ok(())
    }

    /// Read and add the groups of a set; `lost` is the code of a group
    /// in the error state.
    fn read_groups(&mut self, set: Set, lost: Status) -> Result<(u64, u64), Error> {
        let mut leaders = [-1; MAX_THREADS * MAX_KINDS];
        for (slot, g) in leaders.iter_mut().zip(self.open_groups(set)) {
            *slot = g.leader;
        }
        let (mut first, mut second) = (0u64, 0u64);
        for &fd in leaders.iter().filter(|&&fd| fd >= 0) {
            let mut buf = [0u8; READ_LEN + 8];
            let n = self
                .sys
                .read(fd, &mut buf)
                .map_err(|e| Error::os(Status::SystemError, e))?;
            // No bytes: the pinned group is in the error state. Any other
            // length, or another number of events, is never reused.
            let (a, b) = buf
                .get(..n)
                .and_then(parse_group_read)
                .ok_or(Error::new(lost))?;
            first = first.wrapping_add(a);
            second = second.wrapping_add(b);
        }
        Ok((first, second))
    }
}

/// Whether [`refer_to_the_cache`] flushes the lines it loads, so that
/// each load misses the cache as well.
pub const FLUSHES: bool = cfg!(target_arch = "x86_64");

/// Refer to the cache the generic event of references counts: the last
/// level on Intel, the second on AMD, the first on Arm. On x86-64 a line
/// of the stack is flushed from every level and loaded again, a few
/// times, so that each load misses the levels below; on Arm every load
/// is a reference to the first level. A hypervisor that hides the
/// events of the cache, as some of the cloud do, leaves the group at
/// zero, which the check of the opening refuses rather than print zeros.
fn refer_to_the_cache() {
    let line = [0u8; 64];
    for _ in 0..8 {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: clflush and mfence are in every x86-64 processor (SSE2)
        // and run in user space; the address is that of a live local.
        unsafe {
            core::arch::x86_64::_mm_clflush(line.as_ptr());
            core::arch::x86_64::_mm_mfence();
        }
        // SAFETY: the address is that of a live local, aligned for u8.
        let _ = unsafe { core::ptr::read_volatile(line.as_ptr()) };
    }
}

/// Two descriptors for each group, a group for each kind and thread.
fn descriptors(plan: &Plan, threads: usize) -> u32 {
    let kinds = u32::try_from(plan.count.min(MAX_KINDS)).unwrap_or(0);
    let threads = u32::try_from(threads.min(MAX_THREADS)).unwrap_or(0);
    kinds.saturating_mul(threads).saturating_mul(2)
}

impl<S: PerfSys> Backend for Perf<S> {
    fn plan(&mut self, sysfs: &CStr, set: Set) -> Result<u32, Error> {
        if set != Set::Basic {
            return self.plan_set(set);
        }
        let fs = LibcFs {
            root: sysfs.to_bytes(),
        };
        let plan = sysfs::discover(&fs, self.sys.uniform_cores()).map_err(Error::new)?;
        self.set_plan(plan)
    }

    fn open(&mut self, set: Set) -> Result<(), Error> {
        let plan = self.plan.ok_or(Error::new(Status::Internal))?;
        for index in 0..self.nthreads(set) {
            if let Err(e) = self.open_thread(set, &plan, index) {
                self.close_set(set);
                return Err(e);
            }
        }
        // Check that the groups count: a group that finds no free
        // counter is in the error state and reads no bytes, and a group
        // that the hypervisor opens but never counts reads zero. The
        // calling thread runs instructions and branches between the
        // enabling and the read, and refers to the cache in between.
        let check = self
            .each_leader(set, PERF_EVENT_IOC_ENABLE)
            .and_then(|()| {
                if set == Set::Cache {
                    self.sys.refer_to_the_cache();
                }
                self.read_groups(set, Status::NoFreeCounter)
            })
            .and_then(|(first, second)| match set {
                Set::Basic if first == 0 => Err(Error::new(Status::NoCounters)),
                Set::Branches | Set::Cache if first == 0 => {
                    Err(Error::new(Status::EventsNotCounted))
                }
                // The flushed lines missed the cache too.
                Set::Cache if FLUSHES && second == 0 => Err(Error::new(Status::EventsNotCounted)),
                _ => Ok(()),
            })
            .and_then(|()| self.each_leader(set, PERF_EVENT_IOC_DISABLE));
        if let Err(e) = check {
            let _ = self.each_leader(set, PERF_EVENT_IOC_DISABLE);
            self.close_set(set);
            return Err(e);
        }
        Ok(())
    }

    fn enable(&mut self, set: Set) -> Result<(), Error> {
        // A failure midway leaves no group of the set on.
        self.each_leader(set, PERF_EVENT_IOC_ENABLE)
            .inspect_err(|_| {
                let _ = self.each_leader(set, PERF_EVENT_IOC_DISABLE);
            })
    }

    fn disable(&mut self, set: Set) -> Result<(), Error> {
        self.each_leader(set, PERF_EVENT_IOC_DISABLE)
    }

    fn read(&mut self, set: Set) -> Result<(u64, u64), Error> {
        self.read_groups(set, Status::CounterLost)
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
