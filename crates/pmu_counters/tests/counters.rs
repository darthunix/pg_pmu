//! The counters of this machine, where it can count; where it cannot,
//! the code it gives instead.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

mod common;

use common::{counters, open_set, work};
use pmu_counters::Status;
use pmu_counters::counters::{Counts, Set};

const BASIC: u32 = Set::Basic.bit();

#[test]
fn counts_grow_with_the_work() {
    let Some((_one, mut c)) = counters() else {
        return;
    };
    let start = c.acquire(BASIC).expect("acquire");
    std::hint::black_box(work(10_000_000));
    let end = c.read(BASIC).expect("read");
    c.release(BASIC).expect("release");
    let instructions = end.instructions - start.instructions;
    assert!(instructions > 10_000_000, "{start:?} {end:?}");
    assert!(end.cycles > start.cycles, "{start:?} {end:?}");
}

#[test]
fn opening_twice_does_nothing_more() {
    let Some((_one, mut c)) = counters() else {
        return;
    };
    assert_eq!(c.open(Set::Basic), Ok(()));
    assert_eq!(c.plan(c"/sys", Set::Basic), Ok(0));
}

/// On Linux the counters count only while a reference is held.
#[cfg(target_os = "linux")]
#[test]
fn disabled_counters_do_not_count() {
    let Some((_one, mut c)) = counters() else {
        return;
    };
    let before = c.acquire(BASIC).expect("acquire");
    c.release(BASIC).expect("release");
    std::hint::black_box(work(10_000_000));
    let after = c.acquire(BASIC).expect("acquire");
    c.release(BASIC).expect("release");
    // A few instructions of the calls themselves, far from ten million.
    assert!(
        after.instructions - before.instructions < 1_000_000,
        "{before:?} {after:?}"
    );
}

/// Touch every line of 64 MB, more than any cache holds.
fn walk(memory: &mut [u8]) -> u64 {
    let mut sum = 0u64;
    for i in (0..memory.len()).step_by(64) {
        memory[i] = memory[i].wrapping_add(1);
        sum = sum.wrapping_add(u64::from(std::hint::black_box(memory[i])));
    }
    sum
}

/// Take references to the basic set and `set`, which are open, or
/// return `None` where they find no counters together: each opened with
/// the other disabled.
fn acquire_beside(
    c: &mut pmu_counters::counters::Counters<pmu_counters::counters::Platform>,
    set: Set,
) -> Option<Counts> {
    match c.acquire(BASIC | set.bit()) {
        Ok(start) => Some(start),
        Err(e) => {
            assert_eq!(e.status, Status::NoFreeCounter, "{e:?}");
            assert!(
                std::env::var_os("PG_PMU_REQUIRE_EVENTS").is_none(),
                "this machine must count {set:?}, but: {e:?}"
            );
            None
        }
    }
}

/// Count `f` with the basic set and `set`, or return `None` where the
/// machine cannot count the set, or not beside the basic one.
fn with_set(
    c: &mut pmu_counters::counters::Counters<pmu_counters::counters::Platform>,
    set: Set,
    f: impl FnOnce(),
) -> Option<(Counts, Counts)> {
    if !open_set(c, set) {
        return None;
    }
    let mask = BASIC | set.bit();
    let start = acquire_beside(c, set)?;
    f();
    let end = c.read(mask).expect("read");
    c.release(mask).expect("release");
    Some((start, end))
}

/// The branches, where the machine counts them: a loop of ten million
/// turns takes at least as many branches, fewer than its instructions,
/// of which only a few are mispredicted. On macOS they are refused.
#[test]
fn branches_count_where_the_machine_counts_them() {
    let Some((_one, mut c)) = counters() else {
        return;
    };
    let Some((start, end)) = with_set(&mut c, Set::Branches, || {
        std::hint::black_box(work(10_000_000));
    }) else {
        return;
    };
    let branches = end.branches - start.branches;
    let misses = end.branch_misses - start.branch_misses;
    let instructions = end.instructions - start.instructions;
    assert!(
        branches >= 10_000_000 && branches < instructions,
        "{start:?} {end:?}"
    );
    assert!(misses < branches / 10, "{start:?} {end:?}");
    assert_eq!((end.cache_references, end.cache_misses), (0, 0));
}

/// The cache, where the machine counts it: a walk over more memory than
/// any cache holds refers to the cache.
#[test]
fn the_cache_counts_where_the_machine_counts_it() {
    let Some((_one, mut c)) = counters() else {
        return;
    };
    let mut memory = vec![0u8; 64 << 20];
    let Some((start, end)) = with_set(&mut c, Set::Cache, || {
        std::hint::black_box(walk(&mut memory));
    }) else {
        return;
    };
    assert!(
        end.cache_references > start.cache_references,
        "{start:?} {end:?}"
    );
    assert_eq!((end.branches, end.branch_misses), (0, 0));
}

/// A set counts only while a reference to it is held, whatever the
/// references to the others.
#[cfg(target_os = "linux")]
#[test]
fn a_set_without_a_reference_does_not_count() {
    let Some((_one, mut c)) = counters() else {
        return;
    };
    if !open_set(&mut c, Set::Branches) {
        return;
    }
    let branches = Set::Branches.bit();
    let Some(before) = acquire_beside(&mut c, Set::Branches) else {
        return;
    };
    c.release(branches).expect("release");
    std::hint::black_box(work(10_000_000));
    assert_eq!(c.read(branches).unwrap_err().status, Status::Internal);
    let basic = c.read(BASIC).expect("read");
    let after = c.acquire(branches).expect("acquire");
    c.release(BASIC | branches).expect("release");
    assert!(basic.instructions - before.instructions > 10_000_000);
    assert!(
        after.branches - before.branches < 1_000_000,
        "{before:?} {after:?}"
    );
}

/// macOS counts only instructions and cycles for a process.
#[cfg(target_os = "macos")]
#[test]
fn macos_refuses_the_other_sets() {
    let Some((_one, mut c)) = counters() else {
        return;
    };
    for set in [Set::Branches, Set::Cache] {
        assert_eq!(
            c.plan(c"/sys", set).unwrap_err().status,
            Status::EventsNotCounted
        );
        assert_eq!(c.open(set).unwrap_err().status, Status::EventsNotCounted);
        assert!(!c.is_open(set));
    }
}
