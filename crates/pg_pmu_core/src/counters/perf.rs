//! The counters of Linux: one group of two perf events, instructions
//! and cycles, for each kind of cores, on the thread of the process and
//! any CPU, in user space only.
//!
//! Each group is pinned: the kernel keeps it on hardware counters all
//! the time, or puts it into an error state where a read returns no
//! bytes, so counts are never scaled from a part of the time. It is
//! opened disabled and enabled while a reference is held; enabling it
//! again takes it out of an error state. The calls of the system go
//! through [`PerfSys`], so that all of this is tested on any system.

use core::ffi::CStr;

use super::perf_abi::{
    self, PERF_COUNT_HW_CPU_CYCLES, PERF_COUNT_HW_INSTRUCTIONS, PERF_EVENT_IOC_DISABLE,
    PERF_EVENT_IOC_ENABLE, PERF_IOC_FLAG_GROUP, PerfEventAttr,
};
use super::{Backend, Counts};
use crate::sysfs::{self, LibcFs, MAX_KINDS, Plan};
use crate::{Error, Status};

/// The calls of the system the counters make. Errors are `errno`.
pub trait PerfSys {
    /// `perf_event_open` for this thread on any CPU; return the file
    /// descriptor.
    fn open(&mut self, attr: &PerfEventAttr, group_fd: i32) -> Result<i32, i32>;
    fn ioctl(&mut self, fd: i32, request: u64, arg: u64) -> Result<(), i32>;
    /// `read`; return the number of bytes read.
    fn read(&mut self, fd: i32, buf: &mut [u8]) -> Result<usize, i32>;
    fn close(&mut self, fd: i32);
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
    groups: [Group; MAX_KINDS],
}

impl<S: PerfSys> Perf<S> {
    pub const fn with(sys: S) -> Self {
        Perf {
            sys,
            plan: None,
            groups: [CLOSED; MAX_KINDS],
        }
    }

    /// The system, for the tests.
    pub fn sys(&self) -> &S {
        &self.sys
    }

    /// The system, for the tests.
    pub fn sys_mut(&mut self) -> &mut S {
        &mut self.sys
    }

    /// Take a plan made elsewhere, as the tests do; return the number of
    /// descriptors it needs.
    pub fn set_plan(&mut self, plan: Plan) -> u32 {
        self.plan = Some(plan);
        descriptors(&plan)
    }

    fn count(&self) -> usize {
        self.plan.map_or(0, |p| p.count.min(MAX_KINDS))
    }

    fn open_group(&mut self, unit: Option<u32>) -> Result<Group, Error> {
        let typed = unit.is_some();
        let leader = perf_abi::attr(PERF_COUNT_HW_INSTRUCTIONS, unit, true);
        let leader = self
            .sys
            .open(&leader, -1)
            .map_err(|e| open_error(e, typed))?;
        let member = perf_abi::attr(PERF_COUNT_HW_CPU_CYCLES, unit, false);
        match self.sys.open(&member, leader) {
            Ok(member) => Ok(Group { leader, member }),
            Err(e) => {
                self.sys.close(leader);
                Err(open_error(e, typed))
            }
        }
    }

    fn close_all(&mut self) {
        for i in 0..MAX_KINDS {
            if let Some(g) = self.groups.get(i).copied() {
                if g.member >= 0 {
                    self.sys.close(g.member);
                }
                if g.leader >= 0 {
                    self.sys.close(g.leader);
                }
            }
            if let Some(g) = self.groups.get_mut(i) {
                *g = CLOSED;
            }
        }
    }

    fn each_leader(&mut self, request: u64) -> Result<(), Error> {
        for i in 0..self.count() {
            let Some(g) = self.groups.get(i).copied() else {
                return Err(Error::new(Status::Internal));
            };
            self.sys
                .ioctl(g.leader, request, PERF_IOC_FLAG_GROUP)
                .map_err(|e| Error::os(Status::SystemError, e))?;
        }
        Ok(())
    }

    /// Read and add the groups; `lost` is the code of a group in the
    /// error state.
    fn read_groups(&mut self, lost: Status) -> Result<Counts, Error> {
        let mut total = Counts::default();
        for i in 0..self.count() {
            let Some(g) = self.groups.get(i).copied() else {
                return Err(Error::new(Status::Internal));
            };
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

/// Two descriptors for each group.
fn descriptors(plan: &Plan) -> u32 {
    let groups = u32::try_from(plan.count.min(MAX_KINDS)).unwrap_or(0);
    groups.saturating_mul(2)
}

impl<S: PerfSys> Backend for Perf<S> {
    fn plan(&mut self, sysfs: &CStr) -> Result<u32, Error> {
        let fs = LibcFs {
            root: sysfs.to_bytes(),
        };
        let plan = sysfs::discover(&fs, self.sys.uniform_cores()).map_err(Error::new)?;
        Ok(self.set_plan(plan))
    }

    fn open(&mut self) -> Result<(), Error> {
        let plan = self.plan.ok_or(Error::new(Status::Internal))?;
        for i in 0..self.count() {
            let unit = if plan.typed {
                plan.types.get(i).copied()
            } else {
                None
            };
            match self.open_group(unit) {
                Ok(g) => {
                    if let Some(slot) = self.groups.get_mut(i) {
                        *slot = g;
                    }
                }
                Err(e) => {
                    self.close_all();
                    return Err(e);
                }
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
