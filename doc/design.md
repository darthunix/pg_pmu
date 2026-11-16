# pg_pmu design

pg_pmu adds an option `PMU` to `EXPLAIN ANALYZE`. It reports how many
instructions and cycles the processor spent on one statement,
including its parallel workers:

```
EXPLAIN (ANALYZE, PMU, VERBOSE) SELECT count(*) FROM t;
 ...
 Planning PMU: instructions=1762115 cycles=583370
 Execution PMU: instructions=52818896 cycles=9396448
 Workers PMU: instructions=7248083 cycles=1922555
```

The main use is to compare two ways of running the same query: two
plans, two settings, two executors. Once the caches are warm,
instructions repeat almost exactly from run to run, while time and
cycles vary much more. The goals and limitations are in the
[README](../README.md).

This note explains how the module counts one process, how it finds
the statement, and how parallel workers report their part.

## The big picture

The module is loaded with `shared_preload_libraries`. It installs
PostgreSQL hooks and asks for a small fixed piece of shared memory.
The work for one statement looks like this:

```
 backend                                    parallel worker
 ─────────────────────────────────────────  ────────────────────────
 EXPLAIN starts    push a frame
 options checked   open counters, take a
                   cell, pg_pmu.cell = token
 planner           counters read around it
 ── start: read counters + cell ──
 ExecutorStart
 ExecutorRun
   Gather ──── launches (settings copied, ──► ExecutorStart  ┐ counted
               token included)                ExecutorRun     │
   waits ◄──── adds its counts to the cell ── ExecutorFinish  ┘
 ExecutorFinish    (AFTER triggers)
 ── finish: read counters + cell ──
 plan printed      print the differences
 ExecutorEnd       not counted
 EXPLAIN ends      give the cell back,
                   pop the frame
```

Each process counts only itself. The backend's counts are the
difference of two reads around the statement. The workers' counts
arrive through the cell, a slot of shared memory the backend owns for
the statement. The worker part is included in `Execution PMU`, and
`VERBOSE` shows it separately as `Workers PMU`.

The code is in C (`pg/pg_pmu.c`, `pg/workers.c`) and in a Rust crate,
`crates/pmu_counters`; the section "Rust and C" says who does what.

## Counting one process

**Linux.** One `perf_event_open` group for each thread of the
process: instructions and cycles, user space only (no kernel, no
hypervisor), for that thread on any CPU. The group is *pinned*: the
kernel either keeps it on the hardware all the time or puts it into
an error state, so the counts are never scaled estimates. A read
returns both values at once and is checked for length.

The threads are listed from `/proc/self/task` once, when the counters
open, and every event is *inherited*: a thread that a counted thread
starts later gets copies of its groups, which are enabled and
disabled with them, and a read of a group adds in its copies, those
of threads that have ended too. So the groups count the whole
process: a thread that ran before, such as one that an extension
started, and one started during a statement. The list is never made
again, since a thread started later would then have a group of its
own besides its copies, and count twice.

```
 listed at the opening:   T1 ---- group G1      T2 ---- group G2
 started later by T1:     T3 ---- copies of G1, read with G1
```

Inheritance takes in the processes the backend starts once its
counters are open as well, such as the program of `COPY ... PROGRAM`,
while they run in a counting statement; macOS does not count them.
Missed is only a thread that another thread starts while the counters
open, after the list and before its parent's group. A process with
more than 64 threads at the opening is refused (0A000) rather than
counted in part.

A copy is pinned like its group, but the kernel does not report a
copy that has lost its counter: a read adds the copies in whatever
their state. So the error state guards only the groups of the threads
listed at the opening, and a copy can lose counts unseen when another
pinned user takes the counters of its CPU.

Linux before 4.13 refuses inherited events whose group is read at
once. There pg_pmu opens the events without inheritance, and counts
only the threads listed at the opening.

On a hybrid processor (P and E cores) each core type has its own PMU
in `/sys/bus/event_source/devices/*/cpus`. pg_pmu opens a group for
each type and each thread and adds them up, and refuses (0A000) if
the types do not cover all online CPUs, since it would silently miss
work otherwise.

The groups are opened once per process, at the first counting
statement, with their file descriptors, two for each group, taken
from PostgreSQL's budget of external files. Statements take
references to them: the first reference enables them, the last
disables them, so a nested EXPLAIN does not switch off the counters
of the outer one.

**macOS.** `proc_pid_rusage(RUSAGE_INFO_V4)` gives `ri_instructions`
and `ri_cycles` for the whole process, kernel included, without
privileges. A virtual machine reports zero, which pg_pmu checks and
refuses with 0A000.

Linux and macOS counts are not comparable: both count every thread,
but only macOS counts the kernel.

## Finding the statement

A statement can run other queries: functions, triggers, the
parameters of `EXPLAIN EXECUTE`. They all go through the same
executor hooks, so the module must tell the explained statement from
them.

It keeps a *depth*: one more inside the planner and inside each executor
start, run and finish, while an EXPLAIN runs. Each EXPLAIN has a *frame*
that remembers its depth. While the frame counts, every executor start
at that depth is a *candidate*: it gets a record with the counters read
at start and, later, at finish. When EXPLAIN prints a plan, the module
takes the finished record of that plan whose executor has not ended yet,
which is the statement itself. Queries at the same depth that are not
the statement either ended already (parameters) or never finished (an
open cursor).

```
 depth 0   EXPLAIN (frame, depth 0)
 depth 0     SELECT f() ............ candidate: the statement
 depth 1       query inside f() .... deeper, part of the count
```

When one EXPLAIN runs several plans, as with a rule that adds a
second statement, each plan is printed with its own counts.

An EXPLAIN run inside a counting statement gets its own frame above
the first. Frames form a stack.

Planning is counted when the planner hook gets the EXPLAIN's state,
which PostgreSQL passes when EXPLAIN plans the query itself. Plans
from the cache of prepared statements show no planning counts.

The window ends at `ExecutorFinish` (AFTER triggers included). The
executor's end is not counted, because the plan is printed before it.

## Parallel workers

A worker is a separate process with its own counters. It counts its
own execution of the plan, the one that feeds the Gather's tuple
queue, and adds the result to a cell in shared memory. The backend
reads the cell at the start and at the finish of the statement; the
difference is the workers' part.

### Shared memory

```
 "pg_pmu", 16 + 32 * (N + K) bytes, allocated at server start

 ┌────────┬──────────────────────────┬────────────────────┐
 │ header │ N cells, one per backend │ K cells, a pool    │
 │ 16 B   │ by ProcNumber            │ for nested EXPLAIN │
 └────────┴──────────────────────────┴────────────────────┘

 header: N (u32) | K (u32) | zero (u64)

 cell, 32 bytes
 ┌──────────────┬────────┬───────┬──────────┬──────────┐
 │ instructions │ cycles │ state │ failures │ sqlstate │
 │ u64          │ u64    │ u64   │ u32      │ u32      │
 └──────────────┴────────┴───────┴──────────┴──────────┘

 state:  bits 63..20 generation G   bits 19..0 writers W
 token:  bits 63..20 generation G   bits 19..0 cell number + 1
```

N is PostgreSQL's maximum number of backends, K is
`pg_pmu.max_nested` (default 16). With default settings that is about
5 KB. The outermost counting statement of a backend uses the
backend's own cell; a counting EXPLAIN nested inside it takes a cell
from the pool, which the whole server shares, or fails with 53000 if
none is free. An EXPLAIN that a parallel worker runs takes no cell:
a worker has no workers of its own.

The backend passes the cell to its workers as a *token* in the hidden
setting `pg_pmu.cell`. PostgreSQL copies settings into every worker
it starts, so no other channel is needed. Users cannot change the
setting: it is a connection-start setting, so `SET` and `RESET` fail
with 55P02, and a value given at connection start is rejected. A
nested EXPLAIN sets it in a nesting level of the settings, so at its
end the token of the outer statement is back for the outer workers.

### The life of a cell

```
            take                   close (W = 0)
  free ──────────────► taken ─────────────────► emptying
  G even, W = 0        G odd                    G + 1, W = all ones
    ▲                  │  ▲                       │
    │                  │  │ enter: W + 1 if the   │
    │                  └──┘ token's G matches     │
    │                       leave: W - 1          │
    └─────────────────── open ◄── empty (take sums, zero them)
```

W is the number of workers adding to the cell at this moment. Every
change of the state is one compare-and-swap on the state word; the
sums themselves are added and taken with atomic adds and swaps.

- A worker reports by *enter, add, leave*. Entering checks that the
  cell's generation equals the token's. A report with an old token is
  refused and adds nothing.
- The backend gives the cell back by *close, empty, open*. Closing
  fails while a worker is inside (W > 0). After closing, the
  generation has moved on, so any late report is refused.
- The backend reads a cell only when none of its workers can be
  adding: before they start and after they end. W = 0 at the read is
  checked, but a consistent read relies on PostgreSQL's waits.

The generation is there to catch bugs in pg_pmu's own bookkeeping,
not races PostgreSQL could cause: it guarantees that a stale report
can never land in another statement's cell. With 44 bits it wraps
after 2^43 uses of one cell.

### When a cell goes back

At the normal end of a statement all its workers have exited, so the
cell goes back right away.

When the statement fails, the error reaches pg_pmu before PostgreSQL
has waited for the workers, and a failing worker may still report.
So the cell is held apart from the frame (a *hold*) and goes back
from the abort callback of the transaction or subtransaction that
owns it; a subtransaction that commits passes its holds to its parent.
PostgreSQL waits for the workers of a subtransaction before calling
the callback.

A nested statement's cell is handed down: its sums are added to the
hold of the statement around it. So the outer statement includes the
nested one's workers, even when the nested one failed and a function
caught the error.

```
 outer EXPLAIN  (backend cell)  ◄── sums of the nested cell
   function
     nested EXPLAIN  (pool cell, workers report here)
```

### Failures in a worker

A worker that cannot count (for example, no free hardware counter)
records a failure and its SQLSTATE in the cell, then raises the error.
Normally the error fails the whole statement anyway. If a function
catches it, the backend still sees the failure count go up at the
statement's finish and fails with the same SQLSTATE, rather than
print counts without that worker.

An ordinary error in a worker (division by zero, cancel) is not a
failure to count: the worker reports what it counted from its exit
callback.

## Errors

The backend opens its counters when EXPLAIN checks its options, so a
system that cannot count fails before the statement runs.

- 42501: the system forbids counting (Linux `perf_event_paranoid`,
  capabilities, a container's seccomp filter); the hint names these.
- 0A000: no counters here: a virtual machine, a system other than
  Linux and macOS, core types that cannot all be counted.
- 53000: no free hardware counter, no file descriptor, a counter lost
  during the statement, no free cell in the pool.
- XX000: an unexpected error of a system call, or a bug in pg_pmu,
  such as an invalid token or a cell returned while a worker writes to
  it.

After an error of the system the next statement tries again. A cell
that a bug of pg_pmu left taken stays taken until the server restarts,
and the log says so.

## Rust and C

```
 C: pg/*.c                   hooks, frames, the setting, errors;
                             allocates the shared memory and reserves
                             file descriptors from PostgreSQL's budget
        │ extern "C"
        ▼
 Rust: crates/pmu_counters   opens, reads and closes the counters on
                             Linux and macOS; the sysfs rules for core
                             types; the layout of the cells and every
                             step on them
```

C keeps what talks to PostgreSQL. Rust keeps what can be tested
without a server, which is why it exists. A bug in the cell protocol
would show only in a rare order of events between processes, which
SQL tests almost never hit. In Rust, loom runs every order of a few
workers and a backend over the same code, proptest compares random
sequences of steps with a simple model, and Kani proves that each step
keeps a cell in a valid state. The sysfs rules run against generated
machines. C never touches a cell directly.

The crate is `no_std`, does not allocate and never panics: a panic
would abort a PostgreSQL process and restart all sessions. Clippy
denies `unwrap`, indexing and unchecked arithmetic. It refuses to
build without native 64-bit atomics.

Linux's `perf_event_attr` is declared by hand in the crate, so a C
file compares the crate's offsets and constants with
`<linux/perf_event.h>` when the module loads, and refuses to start on
a mismatch. Bit fields have no offsets, so this cannot be done at
compile time.

## Alternatives we did not take

- **Columns in pg_stat_kcache.** It sums all runs of a query; one run
  cannot be seen apart without a reset.
- **`perf stat -p` from outside.** It cannot see where a statement
  begins or ends, and workers are other processes.
- **Counts per node.** Two reads per row per node cost more than the
  work of many nodes.
- **Counting without EXPLAIN.** It would remove the overhead of
  EXPLAIN ANALYZE's own instrumentation, which hurts a row-at-a-time
  executor more than a batch one. We accept the bias and document it;
  `TIMING off` makes it smaller.
- **`LOAD` with dynamic shared memory.** The pool size could not be a
  start-up setting, and a replaced library could meet old memory.
- **One cell for a statement and everything nested in it.** A nested
  statement inside a parallel plan would mix in the outer workers.
- **The protocol in C on PostgreSQL atomics.** No tool explores its
  interleavings.
- **`rdpmc` for reads.** Faster, but a statement reads only a few
  times, and it reads only the calling thread.
- **Only the calling thread.** Simpler, but it misses the threads of
  an extension, and Linux then counts less than macOS.
- **The threads listed at every statement, without inheritance.** It
  would catch the threads started between statements, but miss those
  started and ended inside one.
- **`inherit_thread`, to leave child processes out.** It needs Linux
  5.13, and the processes a backend starts are rare and part of its
  work.

## Limitations

- The counts include EXPLAIN ANALYZE's instrumentation, a few dozen
  instructions per row per node.
- The executor's end, worker start-up and queries a worker runs
  outside the plan (for example in a parallel index build) are not
  counted.
- More nested counting statements at once than `pg_pmu.max_nested`
  fail with 53000.
- On Linux, a copy of a group in a thread started later can lose
  counts unseen; before Linux 4.13 such threads are not counted.
- On a hybrid Intel CPU we assume the kernel skips a pinned group on
  the other core type rather than putting it into the error state.
  This needs checking on such a machine.
