//! The counters of this machine count every thread of the process: the
//! threads that ran when they opened, threads those start later, threads
//! started while the counters were disabled, threads started and ended
//! inside a statement, and on Linux a process started inside one.
//!
//! The binary has one test: the harness runs the tests of a binary at
//! once, in threads of one process, and they would add to the counts.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

mod common;

use std::hint::black_box;
use std::sync::mpsc;
use std::thread;

use common::{counters, work};
use pmu_counters::counters::{Counters, Platform};

const N: u64 = 10_000_000;

/// Where a worker does its work: itself, or in a thread it starts and
/// waits for.
#[derive(Clone, Copy)]
enum Where {
    Here,
    InAThread,
}

/// A thread that does the work each time it is told to.
struct Worker {
    go: mpsc::Sender<Where>,
    done: mpsc::Receiver<()>,
    handle: thread::JoinHandle<()>,
}

impl Worker {
    fn start() -> Self {
        let (go, told) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let handle = thread::spawn(move || {
            while let Ok(place) = told.recv() {
                match place {
                    Where::Here => {
                        black_box(work(N));
                    }
                    Where::InAThread => {
                        thread::spawn(|| black_box(work(N))).join().unwrap();
                    }
                }
                finished.send(()).unwrap();
            }
        });
        Worker { go, done, handle }
    }

    /// Let it work once, and wait until it is done.
    fn run(&self, place: Where) {
        self.go.send(place).unwrap();
        self.done.recv().unwrap();
    }

    fn stop(self) {
        drop(self.go);
        self.handle.join().unwrap();
    }
}

/// The instructions of one statement: a reference taken, `f` run, the
/// counters read, the reference given back.
fn statement(c: &mut Counters<Platform>, f: impl FnOnce()) -> u64 {
    let start = c.acquire().unwrap();
    f();
    let end = c.read().unwrap();
    c.release().unwrap();
    end.instructions - start.instructions
}

#[test]
fn every_thread_of_the_process_is_counted() {
    let early = Worker::start();
    let Some((_one, mut c)) = counters() else {
        early.stop();
        return;
    };
    // The same work in the calling thread: what each case must count.
    let own = statement(&mut c, || {
        black_box(work(N));
    });
    // Far less: the thread was missed; about twice: counted twice.
    let check = |case: &str, counted: u64| {
        assert!(
            counted > own / 10 * 9 && counted < own / 2 * 3,
            "{case}: {counted} instructions, {own} in the calling thread"
        );
    };

    check("a thread that ran when the counters opened", {
        statement(&mut c, || early.run(Where::Here))
    });

    // Linux before 4.13 refuses inheritance, and then counts only the
    // threads listed at the opening.
    #[cfg(all(target_os = "linux", not(feature = "none")))]
    let inherits = c.backend().inherits();
    #[cfg(not(all(target_os = "linux", not(feature = "none"))))]
    let inherits = true;
    if !inherits {
        eprintln!("the kernel refuses inheritance: threads started later are not counted");
        early.stop();
        return;
    }

    check("a thread started by one of those", {
        statement(&mut c, || early.run(Where::InAThread))
    });
    let late = Worker::start();
    check("a thread started while they were disabled", {
        statement(&mut c, || late.run(Where::Here))
    });
    check("a thread started by a thread started later", {
        statement(&mut c, || late.run(Where::InAThread))
    });
    check("a thread started and ended inside", {
        statement(&mut c, || {
            thread::spawn(|| black_box(work(N))).join().unwrap();
        })
    });

    // A process started inside: its work is a shell's loop, some hundred
    // million instructions whatever the shell, while the parent's own
    // part, a start and a wait, is some hundred thousand. The bound does
    // not follow `own`, which a build without optimization makes 15
    // times larger. macOS does not count other processes.
    #[cfg(target_os = "linux")]
    {
        let counted = statement(&mut c, || {
            let status = std::process::Command::new("/bin/sh")
                .args(["-c", "i=0; while [ $i -lt 200000 ]; do i=$((i+1)); done"])
                .status()
                .unwrap();
            assert!(status.success());
        });
        assert!(
            counted > 10_000_000,
            "a process started inside: {counted} instructions"
        );
    }

    early.stop();
    late.stop();
}
