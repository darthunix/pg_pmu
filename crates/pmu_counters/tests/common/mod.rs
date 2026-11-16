//! What the tests of the counters of this machine share.

use pmu_counters::Status;
use pmu_counters::counters::{Counters, Platform, platform};

/// Work the compiler cannot remove.
pub fn work(n: u64) -> u64 {
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
pub fn counters() -> Option<Counters<Platform>> {
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
