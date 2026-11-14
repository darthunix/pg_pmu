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
use pg_pmu_core::counters::{Counters, Platform};

/// Work the compiler cannot remove.
fn work(n: u64) -> u64 {
    let mut x = 1u64;
    for i in 0..n {
        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(i));
    }
    x
}

#[test]
fn counts_grow_with_the_work() {
    let mut c = Counters::new(Platform::new());
    c.plan(c"/sys").expect("plan");
    if let Err(e) = c.open() {
        assert!(
            matches!(
                e.status,
                Status::NoSystem | Status::NoCounters | Status::NotPermitted
            ),
            "{e:?}"
        );
        // A machine that must count says so, and then this fails.
        assert!(
            std::env::var_os("PG_PMU_REQUIRE_COUNTERS").is_none(),
            "this machine must count, but: {e:?}"
        );
        eprintln!("this machine cannot count: {e:?}");
        return;
    }
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
    let mut c = Counters::new(Platform::new());
    let first = c.open();
    assert_eq!(c.open(), first);
    if first.is_ok() {
        assert_eq!(c.plan(c"/sys"), Ok(0));
    }
}
