//! Models of loom: every order of the steps of a few threads on the
//! cells, where workers report while a backend gives a cell back, takes
//! it again or hands a nested cell down. Run with
//! `RUSTFLAGS="--cfg loom" cargo test --release --test loom`.

#![cfg(loom)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use loom::sync::Arc;
use loom::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use loom::thread;

use pg_pmu_core::Status;
use pg_pmu_core::cells::{self, Cell, Contents};

/// A worker's report: enter, add, leave. Whether it was taken.
fn report(cell: &Cell, g: u64, instructions: u64) -> bool {
    match cell.enter(g) {
        Ok(()) => {
            cell.add(instructions, 2 * instructions);
            cell.leave().unwrap();
            true
        }
        Err(Status::StaleToken) => false,
        Err(e) => panic!("a report failed with {e:?}"),
    }
}

/// Try to give a cell back once, as the module does: a worker inside
/// makes it refused as busy, and nothing else may refuse it.
fn try_give_back(cell: &Cell, g: u64) -> Option<Contents> {
    match cell.give_back(g) {
        Ok(c) => Some(c),
        Err(Status::CellBusy) => None,
        Err(e) => panic!("a return failed with {e:?}"),
    }
}

/// Give a cell back when no worker can be inside any more.
fn give_back_after(first: Option<Contents>, cell: &Cell, g: u64) -> Contents {
    first.unwrap_or_else(|| cell.give_back(g).unwrap())
}

fn taken(accepted: bool, instructions: u64) -> u64 {
    if accepted { instructions } else { 0 }
}

/// Two workers report while the backend gives the cell back: what each
/// adds is either in what the backend gets, or refused.
#[test]
fn reports_against_a_close() {
    loom::model(|| {
        let cell = Arc::new(Cell::default());
        let g = cell.take().unwrap();
        let workers: Vec<_> = [1u64, 10]
            .into_iter()
            .map(|n| {
                let cell = cell.clone();
                thread::spawn(move || taken(report(&cell, g, n), n))
            })
            .collect();
        let first = try_give_back(&cell, g);
        let sum: u64 = workers.into_iter().map(|w| w.join().unwrap()).sum();
        let got = give_back_after(first, &cell, g);
        assert_eq!((got.instructions, got.cycles), (sum, 2 * sum));
        assert_eq!(cell.state(), (g + 1, 0));
    });
}

/// A worker reports late with an old token while the backend gives the
/// cell back and takes it again: the report lands in the old use or
/// nowhere, never in the new one.
#[test]
fn a_late_report_against_a_new_taking() {
    loom::model(|| {
        let cell = Arc::new(Cell::default());
        let old = cell.take().unwrap();
        let worker = {
            let cell = cell.clone();
            thread::spawn(move || report(&cell, old, 7))
        };
        // When the worker is inside, the new taking waits for it.
        let first = try_give_back(&cell, old);
        let second = first.map(|_| {
            let new = cell.take().unwrap();
            assert_ne!(new, old);
            cell.give_back(new).unwrap()
        });
        let accepted = worker.join().unwrap();
        let first = give_back_after(first, &cell, old);
        assert_eq!(first.instructions, taken(accepted, 7));
        assert_eq!(second.unwrap_or_default(), Contents::default());
    });
}

/// Two backends race for the cells of a pool of one, each giving its
/// cell back: never both hold it, and a backend that finds none free
/// is told so.
#[test]
fn two_backends_race_for_a_cell_of_the_pool() {
    loom::model(|| {
        let pool = Arc::new([Cell::default()]);
        let holders = Arc::new(AtomicUsize::new(0));
        let backends: Vec<_> = (0..2)
            .map(|_| {
                let pool = pool.clone();
                let holders = holders.clone();
                thread::spawn(move || match cells::take_from_pool(&pool[..]) {
                    Some((i, g)) => {
                        assert_eq!(holders.fetch_add(1, SeqCst), 0);
                        holders.fetch_sub(1, SeqCst);
                        pool[i].give_back(g).unwrap();
                        true
                    }
                    None => false,
                })
            })
            .collect();
        let took: usize = backends
            .into_iter()
            .map(|b| usize::from(b.join().unwrap()))
            .sum();
        assert!(took >= 1);
        assert_eq!(pool[0].state().1, 0);
    });
}

/// The backend hands a nested cell down into the hold below while a
/// worker of the outer statement reports to the outer cell and a late
/// worker of the nested one reports to its cell: the total of the outer
/// statement holds every report taken, once.
#[test]
fn a_nested_cell_handed_down_while_a_report_comes() {
    loom::model(|| {
        let outer = Arc::new(Cell::default());
        let nested = Arc::new(Cell::default());
        let go = outer.take().unwrap();
        let gn = nested.take().unwrap();
        let outer_worker = {
            let outer = outer.clone();
            thread::spawn(move || taken(report(&outer, go, 1), 1))
        };
        let nested_worker = {
            let nested = nested.clone();
            thread::spawn(move || taken(report(&nested, gn, 10), 10))
        };
        // The hold below sums what nested cells hand down.
        let handed = try_give_back(&nested, gn);
        let own = try_give_back(&outer, go);
        let want = outer_worker.join().unwrap() + nested_worker.join().unwrap();
        let handed = give_back_after(handed, &nested, gn).instructions;
        let own = give_back_after(own, &outer, go).instructions;
        assert_eq!(own + handed, want);
        assert_eq!(nested.read(gn), Err(Status::StaleToken));
    });
}

/// Two returns of one cell, a fault of the module: one gives it back,
/// the other is told the cell is gone, and the cell is free once.
#[test]
fn two_returns_of_one_cell() {
    loom::model(|| {
        let cell = Arc::new(Cell::default());
        let g = cell.take().unwrap();
        let other = {
            let cell = cell.clone();
            thread::spawn(move || cell.give_back(g).map(|_| ()))
        };
        let mine = cell.give_back(g).map(|_| ());
        let other = other.join().unwrap();
        let mut results = [mine, other];
        results.sort_by_key(|r| r.is_err());
        assert_eq!(results, [Ok(()), Err(Status::StaleToken)]);
        assert_eq!(cell.state(), (g + 1, 0));
    });
}
