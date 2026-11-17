//! The counters of the processor for one process.
//!
//! The counters come in [`Set`]s, pairs of events counted together:
//! instructions and cycles, which every counting statement counts, and
//! the branches and the cache, which a statement counts when it asks.
//! A [`Backend`] is the system's way to count a set: perf events on
//! Linux, the counters of the process on macOS, or none. [`Counters`]
//! keeps the order of the steps and the references: every counting
//! statement and every counting execution of a worker takes a reference
//! to each set it counts, a set is enabled while at least one reference
//! to it is held, and its values only grow while it is enabled.

#[cfg(all(target_os = "linux", not(feature = "none")))]
mod linux;
#[cfg(all(target_os = "macos", not(feature = "none")))]
mod macos;
pub mod perf;
pub mod perf_abi;
// Unused where the system counts, but always built so that it stays right.
#[cfg_attr(
    all(any(target_os = "linux", target_os = "macos"), not(feature = "none")),
    allow(dead_code)
)]
mod none;

use core::ffi::CStr;

use crate::{Error, Status};

/// A pair of events the crate counts in groups of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Set {
    /// Instructions and cycles, which every counting statement counts.
    Basic,
    /// Branches and the branches the processor mispredicted.
    Branches,
    /// References to the cache and misses of the cache, as the kernel's
    /// generic events name them: which level of the cache they count
    /// depends on the processor.
    Cache,
}

/// The number of sets.
pub const SETS: usize = 3;

/// The bits of every set in a mask.
pub const ALL_SETS: u32 = 0b111;

impl Set {
    /// Every set, in the order the crate opens, reads and adds them.
    pub const ALL: [Set; SETS] = [Set::Basic, Set::Branches, Set::Cache];

    /// The bit of the set in a mask, as `pmu_counters.h` defines it.
    pub const fn bit(self) -> u32 {
        match self {
            Set::Basic => 1,
            Set::Branches => 2,
            Set::Cache => 4,
        }
    }

    /// The place of the set in [`Set::ALL`].
    pub const fn index(self) -> usize {
        match self {
            Set::Basic => 0,
            Set::Branches => 1,
            Set::Cache => 2,
        }
    }

    /// The set of a mask with exactly its bit.
    pub fn of_bit(bit: u32) -> Option<Set> {
        Set::ALL.into_iter().find(|s| s.bit() == bit)
    }
}

/// The sets of a mask, in the order of [`Set::ALL`], or `None` for a
/// mask with a bit of no set.
pub fn sets_in(mask: u32) -> Option<impl Iterator<Item = Set>> {
    if mask & !ALL_SETS != 0 {
        return None;
    }
    Some(Set::ALL.into_iter().filter(move |s| mask & s.bit() != 0))
}

/// The values counted so far, two of each set; those of a set that was
/// not read are 0. Only differences of two readings mean something.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub instructions: u64,
    pub cycles: u64,
    pub branches: u64,
    pub branch_misses: u64,
    pub cache_references: u64,
    pub cache_misses: u64,
}

/// The number of values in [`Counts`].
pub const VALUES: usize = 6;

impl Counts {
    /// The two values of a set.
    pub const fn pair(&self, set: Set) -> (u64, u64) {
        match set {
            Set::Basic => (self.instructions, self.cycles),
            Set::Branches => (self.branches, self.branch_misses),
            Set::Cache => (self.cache_references, self.cache_misses),
        }
    }

    /// Set the two values of a set.
    pub fn set_pair(&mut self, set: Set, (first, second): (u64, u64)) {
        let (a, b) = match set {
            Set::Basic => (&mut self.instructions, &mut self.cycles),
            Set::Branches => (&mut self.branches, &mut self.branch_misses),
            Set::Cache => (&mut self.cache_references, &mut self.cache_misses),
        };
        *a = first;
        *b = second;
    }

    /// The values in the order of the fields.
    pub const fn to_array(self) -> [u64; VALUES] {
        [
            self.instructions,
            self.cycles,
            self.branches,
            self.branch_misses,
            self.cache_references,
            self.cache_misses,
        ]
    }

    /// The counts of values in the order of the fields.
    pub const fn from_array(v: [u64; VALUES]) -> Self {
        let [
            instructions,
            cycles,
            branches,
            branch_misses,
            cache_references,
            cache_misses,
        ] = v;
        Counts {
            instructions,
            cycles,
            branches,
            branch_misses,
            cache_references,
            cache_misses,
        }
    }
}

/// The system's way to count. Every function returns an error rather
/// than panic.
pub trait Backend {
    /// Find what must be opened for a set, and return how many file
    /// descriptors the opening needs. It may be called again while the
    /// set is not open. `sysfs` is the root of sysfs, `/sys` outside the
    /// tests. [`Counters`] plans a set other than the basic one only once
    /// the basic one is open.
    fn plan(&mut self, sysfs: &CStr, set: Set) -> Result<u32, Error>;
    /// Open the counters of a set, disabled, and check that they count.
    /// On an error nothing of the set stays open.
    fn open(&mut self, set: Set) -> Result<(), Error>;
    /// Let the counters of a set count.
    fn enable(&mut self, set: Set) -> Result<(), Error>;
    /// Stop the counters of a set.
    fn disable(&mut self, set: Set) -> Result<(), Error>;
    /// Read the two values of a set counted so far.
    fn read(&mut self, set: Set) -> Result<(u64, u64), Error>;
}

/// The backend of this build.
#[cfg(all(target_os = "linux", not(feature = "none")))]
pub type Platform = perf::Perf<linux::LinuxSys>;
/// The backend of this build.
#[cfg(all(target_os = "macos", not(feature = "none")))]
pub type Platform = macos::Rusage;
/// The backend of this build.
#[cfg(not(all(any(target_os = "linux", target_os = "macos"), not(feature = "none"))))]
pub type Platform = none::Unsupported;

/// Make the backend of this build.
pub const fn platform() -> Platform {
    #[cfg(all(target_os = "linux", not(feature = "none")))]
    {
        perf::Perf::with(linux::LinuxSys::new())
    }
    #[cfg(all(target_os = "macos", not(feature = "none")))]
    {
        macos::Rusage::new()
    }
    #[cfg(not(all(any(target_os = "linux", target_os = "macos"), not(feature = "none"))))]
    {
        none::Unsupported::new()
    }
}

/// The counters of one process and the references to each set.
pub struct Counters<B> {
    backend: B,
    open: [bool; SETS],
    refs: [u32; SETS],
}

impl<B: Backend> Counters<B> {
    pub const fn new(backend: B) -> Self {
        Counters {
            backend,
            open: [false; SETS],
            refs: [0; SETS],
        }
    }

    /// Whether the counters of a set are open.
    pub fn is_open(&self, set: Set) -> bool {
        self.open.get(set.index()).copied().unwrap_or(false)
    }

    /// The references to a set held now.
    pub fn refs(&self, set: Set) -> u32 {
        self.refs.get(set.index()).copied().unwrap_or(0)
    }

    /// The backend, for the tests.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// The backend, for the tests.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// The sets other than the basic one are opened after it.
    fn needs_basic(&self, set: Set) -> Result<(), Error> {
        if set != Set::Basic && !self.is_open(Set::Basic) {
            return Err(Error::new(Status::Internal));
        }
        Ok(())
    }

    /// How many file descriptors the opening of a set needs: 0 once it
    /// is open.
    pub fn plan(&mut self, sysfs: &CStr, set: Set) -> Result<u32, Error> {
        if self.is_open(set) {
            return Ok(0);
        }
        self.needs_basic(set)?;
        self.backend.plan(sysfs, set)
    }

    /// Open the counters of a set once in the process; later calls do
    /// nothing.
    pub fn open(&mut self, set: Set) -> Result<(), Error> {
        if self.is_open(set) {
            return Ok(());
        }
        self.needs_basic(set)?;
        self.backend.open(set)?;
        let slot = self
            .open
            .get_mut(set.index())
            .ok_or(Error::new(Status::Internal))?;
        *slot = true;
        Ok(())
    }

    /// Take a reference to each set of a mask and read them. The first
    /// reference to a set enables it. On an error no reference is taken.
    pub fn acquire(&mut self, mask: u32) -> Result<Counts, Error> {
        let sets = sets_in(mask).ok_or(Error::new(Status::Internal))?;
        let mut counts = Counts::default();
        let mut taken = 0;
        for set in sets {
            match self.acquire_one(set) {
                Ok(pair) => {
                    counts.set_pair(set, pair);
                    taken |= set.bit();
                }
                Err(error) => {
                    // The error of the acquire is the one to report.
                    for s in sets_in(taken).into_iter().flatten() {
                        let _ = self.release_one(s);
                    }
                    return Err(error);
                }
            }
        }
        Ok(counts)
    }

    fn acquire_one(&mut self, set: Set) -> Result<(u64, u64), Error> {
        if !self.is_open(set) {
            return Err(Error::new(Status::Internal));
        }
        let held = self.refs(set);
        let refs = held.checked_add(1).ok_or(Error::new(Status::Internal))?;
        if held == 0 {
            self.backend.enable(set)?;
        }
        match self.backend.read(set) {
            Ok(pair) => {
                let slot = self
                    .refs
                    .get_mut(set.index())
                    .ok_or(Error::new(Status::Internal))?;
                *slot = refs;
                Ok(pair)
            }
            Err(mut error) => {
                if held == 0 {
                    // The read already failed; that error is the one to
                    // report. A counter lost right after it was enabled
                    // was never free.
                    let _ = self.backend.disable(set);
                    if error.status == Status::CounterLost {
                        error.status = Status::NoFreeCounter;
                    }
                }
                Err(error)
            }
        }
    }

    /// Read the sets of a mask while a reference to each is held.
    pub fn read(&mut self, mask: u32) -> Result<Counts, Error> {
        let sets = sets_in(mask).ok_or(Error::new(Status::Internal))?;
        let mut counts = Counts::default();
        for set in sets {
            if self.refs(set) == 0 {
                return Err(Error::new(Status::Internal));
            }
            counts.set_pair(set, self.backend.read(set)?);
        }
        Ok(counts)
    }

    /// Give back a reference to each set of a mask. The last one to a
    /// set disables it; every reference is given back even when
    /// disabling fails, and the first error is returned. A set without a
    /// reference makes it an internal error that gives nothing back.
    pub fn release(&mut self, mask: u32) -> Result<(), Error> {
        let sets = sets_in(mask).ok_or(Error::new(Status::Internal))?;
        if sets_in(mask)
            .into_iter()
            .flatten()
            .any(|s| self.refs(s) == 0)
        {
            return Err(Error::new(Status::Internal));
        }
        let mut result = Ok(());
        for set in sets {
            if let Err(e) = self.release_one(set)
                && result.is_ok()
            {
                result = Err(e);
            }
        }
        result
    }

    fn release_one(&mut self, set: Set) -> Result<(), Error> {
        let slot = self
            .refs
            .get_mut(set.index())
            .ok_or(Error::new(Status::Internal))?;
        *slot = slot.checked_sub(1).ok_or(Error::new(Status::Internal))?;
        if *slot == 0 {
            self.backend.disable(set)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing
    )]

    use super::*;

    /// A backend that counts its calls for each set and can be told to
    /// fail.
    #[derive(Default)]
    struct Fake {
        enabled: [bool; SETS],
        enables: [u32; SETS],
        disables: [u32; SETS],
        fail_read: Option<Set>,
        fail_enable: bool,
        fail_disable: bool,
    }

    impl Backend for Fake {
        fn plan(&mut self, _: &CStr, _: Set) -> Result<u32, Error> {
            Ok(2)
        }
        fn open(&mut self, _: Set) -> Result<(), Error> {
            Ok(())
        }
        fn enable(&mut self, set: Set) -> Result<(), Error> {
            if self.fail_enable {
                return Err(Error::os(Status::SystemError, 5));
            }
            self.enabled[set.index()] = true;
            self.enables[set.index()] += 1;
            Ok(())
        }
        fn disable(&mut self, set: Set) -> Result<(), Error> {
            self.enabled[set.index()] = false;
            self.disables[set.index()] += 1;
            if self.fail_disable {
                return Err(Error::os(Status::SystemError, 5));
            }
            Ok(())
        }
        fn read(&mut self, set: Set) -> Result<(u64, u64), Error> {
            if self.fail_read == Some(set) {
                Err(Error::new(Status::CounterLost))
            } else {
                Ok((set.bit().into(), 0))
            }
        }
    }

    const BASIC: u32 = Set::Basic.bit();
    const BRANCHES: u32 = Set::Branches.bit();
    const CACHE: u32 = Set::Cache.bit();

    #[test]
    fn first_reference_enables_and_last_disables() {
        let mut c = Counters::new(Fake::default());
        assert_eq!(c.plan(c"/sys", Set::Basic).unwrap(), 2);
        c.open(Set::Basic).unwrap();
        assert_eq!(c.plan(c"/sys", Set::Basic).unwrap(), 0);
        c.acquire(BASIC).unwrap();
        c.acquire(BASIC).unwrap();
        assert_eq!(c.backend().enables[0], 1);
        c.release(BASIC).unwrap();
        assert!(c.backend().enabled[0]);
        c.release(BASIC).unwrap();
        assert!(!c.backend().enabled[0]);
        assert_eq!(c.backend().disables[0], 1);
    }

    #[test]
    fn steps_out_of_order_are_internal_errors() {
        let mut c = Counters::new(Fake::default());
        assert_eq!(c.acquire(BASIC).unwrap_err().status, Status::Internal);
        // The other sets open after the basic one.
        assert_eq!(
            c.plan(c"/sys", Set::Branches).unwrap_err().status,
            Status::Internal
        );
        assert_eq!(c.open(Set::Cache).unwrap_err().status, Status::Internal);
        c.open(Set::Basic).unwrap();
        assert_eq!(c.read(BASIC).unwrap_err().status, Status::Internal);
        assert_eq!(c.release(BASIC).unwrap_err().status, Status::Internal);
        // A set that is not open, and bits of no set.
        assert_eq!(
            c.acquire(BASIC | BRANCHES).unwrap_err().status,
            Status::Internal
        );
        assert_eq!(c.refs(Set::Basic), 0);
        assert_eq!(c.acquire(8).unwrap_err().status, Status::Internal);
    }

    #[test]
    fn each_set_has_its_own_references() {
        let mut c = Counters::new(Fake::default());
        c.open(Set::Basic).unwrap();
        c.open(Set::Branches).unwrap();
        c.open(Set::Cache).unwrap();
        let counts = c.acquire(BASIC | CACHE).unwrap();
        assert_eq!(counts.pair(Set::Basic), (1, 0));
        assert_eq!(counts.pair(Set::Branches), (0, 0));
        assert_eq!(counts.pair(Set::Cache), (4, 0));
        assert_eq!(c.backend().enabled, [true, false, true]);
        c.acquire(BASIC | BRANCHES).unwrap();
        assert_eq!(c.backend().enabled, [true, true, true]);
        // Every set has a reference now.
        assert_eq!(c.read(ALL_SETS).unwrap().branches, 2);
        c.release(BASIC | CACHE).unwrap();
        assert_eq!(c.backend().enabled, [true, true, false]);
        assert_eq!(c.read(CACHE).unwrap_err().status, Status::Internal);
        // Nothing is given back when one set of the mask has no reference.
        assert_eq!(c.release(ALL_SETS).unwrap_err().status, Status::Internal);
        assert_eq!((c.refs(Set::Basic), c.refs(Set::Branches)), (1, 1));
        c.release(BASIC | BRANCHES).unwrap();
        assert_eq!(c.backend().enabled, [false, false, false]);
    }

    #[test]
    fn failed_first_acquire_leaves_nothing_enabled() {
        let mut c = Counters::new(Fake {
            fail_read: Some(Set::Basic),
            ..Fake::default()
        });
        c.open(Set::Basic).unwrap();
        // Lost right after it was enabled: it was never free.
        assert_eq!(c.acquire(BASIC).unwrap_err().status, Status::NoFreeCounter);
        assert_eq!(c.refs(Set::Basic), 0);
        assert!(!c.backend().enabled[0]);
    }

    #[test]
    fn a_failed_set_gives_back_the_sets_before_it() {
        let mut c = Counters::new(Fake {
            fail_read: Some(Set::Cache),
            ..Fake::default()
        });
        for set in Set::ALL {
            c.open(set).unwrap();
        }
        c.acquire(BASIC).unwrap();
        assert_eq!(
            c.acquire(ALL_SETS).unwrap_err().status,
            Status::NoFreeCounter
        );
        // The basic set keeps the reference taken before.
        assert_eq!(
            Set::ALL.map(|s| c.refs(s)),
            [1, 0, 0],
            "references after the failure"
        );
        assert_eq!(c.backend().enabled, [true, false, false]);
    }

    #[test]
    fn failed_enable_takes_no_reference() {
        let mut c = Counters::new(Fake {
            fail_enable: true,
            ..Fake::default()
        });
        c.open(Set::Basic).unwrap();
        assert_eq!(c.acquire(BASIC).unwrap_err().status, Status::SystemError);
        assert_eq!(c.refs(Set::Basic), 0);
    }

    #[test]
    fn reference_is_given_back_when_disabling_fails() {
        let mut c = Counters::new(Fake::default());
        c.open(Set::Basic).unwrap();
        c.open(Set::Branches).unwrap();
        c.acquire(BASIC | BRANCHES).unwrap();
        c.backend_mut().fail_disable = true;
        assert_eq!(
            c.release(BASIC | BRANCHES).unwrap_err().status,
            Status::SystemError
        );
        assert_eq!((c.refs(Set::Basic), c.refs(Set::Branches)), (0, 0));
        assert_eq!(c.backend().disables, [1, 1, 0]);
    }

    #[test]
    fn pairs_and_arrays_read_back() {
        let mut counts = Counts::default();
        for (i, set) in Set::ALL.into_iter().enumerate() {
            counts.set_pair(set, (2 * i as u64 + 1, 2 * i as u64 + 2));
        }
        assert_eq!(counts.to_array(), [1, 2, 3, 4, 5, 6]);
        assert_eq!(Counts::from_array(counts.to_array()), counts);
        assert_eq!(counts.pair(Set::Cache), (5, 6));
        for set in Set::ALL {
            assert_eq!(Set::of_bit(set.bit()), Some(set));
            assert_eq!(Set::ALL[set.index()], set);
        }
        assert_eq!(Set::of_bit(3), None);
        assert!(sets_in(ALL_SETS + 1).is_none());
        assert_eq!(
            sets_in(5).unwrap().collect::<Vec<_>>(),
            [Set::Basic, Set::Cache]
        );
    }
}
