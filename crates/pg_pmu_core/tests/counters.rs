//! The counters of this machine, where it can count; where it cannot,
//! the code it gives instead.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use pg_pmu_core::Status;
use pg_pmu_core::counters::{Counters, platform};

/// Work the compiler cannot remove.
fn work(n: u64) -> u64 {
    let mut x = 1u64;
    for i in 0..n {
        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(i));
    }
    x
}

/// The codes of a machine that cannot count.
fn cannot_count(status: Status) -> bool {
    matches!(
        status,
        Status::NoSystem
            | Status::NoCounters
            | Status::NotPermitted
            | Status::CoresNotCovered
            | Status::KindNotOpenable
            | Status::TooManyKinds
            | Status::NoFreeCounter
    )
}

/// Plan and open the counters of this machine, or return `None` where
/// it cannot count, unless PG_PMU_REQUIRE_COUNTERS says it must.
fn counters() -> Option<Counters<pg_pmu_core::counters::Platform>> {
    let mut c = Counters::new(platform());
    match c.plan(c"/sys").and_then(|_| c.open()) {
        Ok(()) => Some(c),
        Err(e) => {
            assert!(cannot_count(e.status), "{e:?}");
            assert!(
                std::env::var_os("PG_PMU_REQUIRE_COUNTERS").is_none(),
                "this machine must count, but: {e:?}"
            );
            eprintln!("this machine cannot count: {e:?}");
            None
        }
    }
}

#[test]
fn counts_grow_with_the_work() {
    let Some(mut c) = counters() else {
        return;
    };
    let start = c.acquire().expect("acquire");
    std::hint::black_box(work(10_000_000));
    let end = c.read().expect("read");
    c.release().expect("release");
    let instructions = end.instructions - start.instructions;
    assert!(instructions > 10_000_000, "{start:?} {end:?}");
    assert!(end.cycles > start.cycles, "{start:?} {end:?}");
}

#[test]
fn opening_twice_does_nothing_more() {
    let Some(mut c) = counters() else {
        return;
    };
    assert_eq!(c.open(), Ok(()));
    assert_eq!(c.plan(c"/sys"), Ok(0));
}

/// On Linux the counters count only while a reference is held.
#[cfg(target_os = "linux")]
#[test]
fn disabled_counters_do_not_count() {
    let Some(mut c) = counters() else {
        return;
    };
    let before = c.acquire().expect("acquire");
    c.release().expect("release");
    std::hint::black_box(work(10_000_000));
    let after = c.acquire().expect("acquire");
    c.release().expect("release");
    // A few instructions of the calls themselves, far from ten million.
    assert!(
        after.instructions - before.instructions < 1_000_000,
        "{before:?} {after:?}"
    );
}
