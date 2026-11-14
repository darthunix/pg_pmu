//! Models of loom: every order of the steps of a few threads on the
//! cells, where workers report while a backend gives a cell back, takes
//! it again or hands a nested cell down. The steps of a backend run in a
//! thread of their own, so that a worker may come before them. Run with
//! `RUSTFLAGS="--cfg loom" cargo test --release --test loom`.

#![cfg(loom)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use loom::sync::Arc;
use loom::thread;

use pmu_counters::Status;
use pmu_counters::cells::{self, Cell, Contents};

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

/// A worker that could not count reports its failure.
fn report_failure(cell: &Cell, g: u64, sqlstate: u32) -> bool {
    match cell.enter(g) {
        Ok(()) => {
            cell.fail(sqlstate);
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

/// A worker reports counts and another its failure while the backend
/// gives the cell back: each report is either in what the backend gets,
/// at once or after the workers, or refused.
#[test]
fn reports_against_a_close() {
    loom::model(|| {
        let cell = Arc::new(Cell::default());
        let g = cell.take().unwrap();
        let counting = {
            let cell = cell.clone();
            thread::spawn(move || taken(report(&cell, g, 10), 10))
        };
        let failing = {
            let cell = cell.clone();
            thread::spawn(move || report_failure(&cell, g, 53))
        };
        let backend = {
            let cell = cell.clone();
            thread::spawn(move || try_give_back(&cell, g))
        };
        let first = backend.join().unwrap();
        let sum = counting.join().unwrap();
        let failed = failing.join().unwrap();
        let got = give_back_after(first, &cell, g);
        assert_eq!((got.instructions, got.cycles), (sum, 2 * sum));
        assert_eq!(
            (got.failures, got.sqlstate),
            if failed { (1, 53) } else { (0, 0) }
        );
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
        let backend = {
            let cell = cell.clone();
            thread::spawn(move || {
                // When the worker is inside, the new taking waits for it.
                let first = try_give_back(&cell, old);
                let second = first.map(|_| {
                    let new = cell.take().unwrap();
                    assert_ne!(new, old);
                    cell.give_back(new).unwrap()
                });
                (first, second)
            })
        };
        let (first, second) = backend.join().unwrap();
        let accepted = worker.join().unwrap();
        let first = give_back_after(first, &cell, old);
        assert_eq!(first.instructions, taken(accepted, 7));
        assert_eq!(second.unwrap_or_default(), Contents::default());
    });
}

/// Two backends take cells of a pool of two at once: both get one, and
/// not the same, as the one that loses a race tries the next cell.
#[test]
fn two_backends_take_from_a_pool() {
    loom::model(|| {
        let pool = Arc::new([Cell::default(), Cell::default()]);
        let backends: Vec<_> = (0..2)
            .map(|_| {
                let pool = pool.clone();
                thread::spawn(move || cells::take_from_pool(&pool[..]).unwrap().0)
            })
            .collect();
        let got: Vec<usize> = backends.into_iter().map(|b| b.join().unwrap()).collect();
        assert_ne!(got[0], got[1]);
    });
}

/// One backend gives back a cell of a pool of one while another takes
/// from the pool, reports to what it took and gives it back: each gets
/// exactly its own counts, as nobody takes a cell while it is emptied.
#[test]
fn a_taking_against_an_emptying() {
    loom::model(|| {
        let pool = Arc::new([Cell::default()]);
        let g = pool[0].take().unwrap();
        assert!(report(&pool[0], g, 5));
        let other = {
            let pool = pool.clone();
            thread::spawn(move || {
                cells::take_from_pool(&pool[..]).map(|(i, g)| {
                    assert!(report(&pool[i], g, 10));
                    pool[i].give_back(g).unwrap().instructions
                })
            })
        };
        let mine = pool[0].give_back(g).unwrap().instructions;
        assert_eq!(mine, 5);
        assert!(matches!(other.join().unwrap(), None | Some(10)));
        assert_eq!(pool[0].state().1, 0);
    });
}

/// The backend hands a nested cell down into the hold below while a
/// worker of the outer statement reports to the outer cell and a late
/// worker of the nested one reports to its cell: the total of the outer
/// statement holds every report taken, once. The hold below is the
/// module's, so here it is a sum in the test.
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
        let returns: Vec<_> = (0..2)
            .map(|_| {
                let cell = cell.clone();
                thread::spawn(move || cell.give_back(g).map(|_| ()))
            })
            .collect();
        let mut results: Vec<_> = returns.into_iter().map(|r| r.join().unwrap()).collect();
        results.sort_by_key(|r| r.is_err());
        assert_eq!(results, [Ok(()), Err(Status::StaleToken)]);
        assert_eq!(cell.state(), (g + 1, 0));
    });
}
