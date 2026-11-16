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

use common::{counters, work};

#[test]
fn counts_grow_with_the_work() {
    let Some((_one, mut c)) = counters() else {
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
    let Some((_one, mut c)) = counters() else {
        return;
    };
    assert_eq!(c.open(), Ok(()));
    assert_eq!(c.plan(c"/sys"), Ok(0));
}

/// On Linux the counters count only while a reference is held.
#[cfg(target_os = "linux")]
#[test]
fn disabled_counters_do_not_count() {
    let Some((_one, mut c)) = counters() else {
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
