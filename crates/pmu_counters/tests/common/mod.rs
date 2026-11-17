//! What the tests of the counters of this machine share.

use std::sync::{Mutex, MutexGuard};

use pmu_counters::Status;
use pmu_counters::counters::{Counters, Platform, Set, platform};

/// One test of a binary counts at a time. Counters of the whole process
/// open groups on the threads of the other tests too, and groups of two
/// tests enabled on one thread at once can run out of hardware counters.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

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

/// Plan and open the counters of this machine, with the lock that the
/// test holds while it counts, or return `None` where it cannot count,
/// unless PG_PMU_REQUIRE_COUNTERS says it must.
pub fn counters() -> Option<(MutexGuard<'static, ()>, Counters<Platform>)> {
    // A test that failed holding the lock leaves nothing to protect.
    let one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut c = Counters::new(platform());
    match c.plan(c"/sys", Set::Basic).and_then(|_| c.open(Set::Basic)) {
        Ok(()) => Some((one, c)),
        Err(e) => {
            assert!(cannot_count(e.status), "{e:?}");
            // Counting the other sets needs the basic one first.
            assert!(
                std::env::var_os("PG_PMU_REQUIRE_COUNTERS").is_none()
                    && std::env::var_os("PG_PMU_REQUIRE_EVENTS").is_none(),
                "this machine must count, but: {e:?}"
            );
            eprintln!("this machine cannot count: {e:?}");
            None
        }
    }
}

/// Plan and open a set other than the basic one, or return false where
/// the machine cannot count it, unless PG_PMU_REQUIRE_EVENTS says it
/// must. macOS never counts them.
#[allow(dead_code)] // Not every binary of tests opens them.
pub fn open_set(c: &mut Counters<Platform>, set: Set) -> bool {
    match c.plan(c"/sys", set).and_then(|_| c.open(set)) {
        Ok(()) => true,
        Err(e) => {
            assert!(
                matches!(
                    e.status,
                    Status::EventsNotCounted | Status::NoFreeCounter | Status::NotPermitted
                ),
                "{e:?}"
            );
            if cfg!(target_os = "macos") {
                assert_eq!(e.status, Status::EventsNotCounted);
            }
            assert!(
                std::env::var_os("PG_PMU_REQUIRE_EVENTS").is_none(),
                "this machine must count {set:?}, but: {e:?}"
            );
            eprintln!("this machine cannot count {set:?}: {e:?}");
            false
        }
    }
}
