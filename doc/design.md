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
cycles vary much more (see [observations.md](observations.md)). The
goals and limitations are in the [README](../README.md).

This note explains how the module finds the statement, how it counts,
how parallel workers report, and how all of it is tested.

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

The code is in C (`pg_pmu.c`, `workers.c`) and in a Rust crate,
`crates/pmu_counters`; the section "Rust and C" says who does what.

## Counting one process

**Linux.** One `perf_event_open` group per process: instructions and
cycles, user space only (no kernel, no hypervisor), for the thread of
the process on any CPU. The group is *pinned*: the kernel either keeps
it on the hardware all the time or puts it into an error state, so the
counts are never scaled estimates. A read returns both values at once
and is checked for length.

On a hybrid processor (P and E cores) each core type has its own PMU
in `/sys/bus/event_source/devices/*/cpus`. pg_pmu opens one group per
type and adds them up, and refuses (0A000) if the types do not cover
all online CPUs, since it would silently miss work otherwise.

The group is opened once per process, at the first counting statement,
with its file descriptors taken from PostgreSQL's budget of external
files. Statements take references to it: the first reference enables
it, the last disables it, so a nested EXPLAIN does not switch off the
counters of the outer one.

**macOS.** `proc_pid_rusage(RUSAGE_INFO_V4)` gives `ri_instructions`
and `ri_cycles` for the whole process, kernel included, without
privileges. A virtual machine reports zero, which pg_pmu checks and
refuses with 0A000.

Linux and macOS counts are not comparable: one counts the kernel and
all threads, the other neither.

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
 C: pg_pmu.c, workers.c      hooks, frames, the setting, errors;
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
without a server, which is why it exists: the cell protocol runs
under loom, proptest and Kani, and the sysfs rules against generated
machines (see Testing). C never touches a cell directly.

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
  times.

## Limitations

- The counts include EXPLAIN ANALYZE's instrumentation, a few dozen
  instructions per row per node.
- The executor's end, worker start-up and queries a worker runs
  outside the plan (for example in a parallel index build) are not
  counted.
- More nested counting statements at once than `pg_pmu.max_nested`
  fail with 53000.
- On a hybrid Intel CPU we assume the kernel skips a pinned group on
  the other core type rather than putting it into the error state.
  This needs checking on such a machine.

## Testing

**SQL suites** run on a temporary server (`pg_pmu.conf`: preloaded,
`max_nested = 2`). Counts change between runs, so the suites check
facts, not values: positive, larger for more rows, the workers' part
within the execution. Numbers in plans are masked. A system that
cannot count prints "cannot count", and the suite `counters` has a
second expected output for that.

- `sql/option.sql`: the option, auto_explain, the setting
  `pg_pmu.cell`, the size of the shared memory.
- `sql/counters.sql`: counts, formats, planning, rules, prepared
  statements, nesting, the pool, parallel workers and their failures.
- `sql/unloaded.sql`: a server without the module.
- `sql/paranoid.sql`: Linux refusing at `perf_event_paranoid` 4.
- `sql/vm.sql`: a macOS virtual machine.
- `sql/workers_cannot_count.sql`: a test build whose workers fail to
  open their counters (`PG_PMU_TEST=workers-cannot-count`).

The main cases are marked `-- case: <name>` in the suites.

**Rust checks** in `crates/pmu_counters`: unit tests of the layout;
proptest against simple models of the cells, of the sysfs rules over
random machines and of the reference counting; loom models of workers
reporting while a backend gives a cell back, takes it again or hands
it down; Kani proofs that every step keeps the cell in a valid state
and that tokens and their decimal form round-trip, and bounded proofs
that the parsers of sysfs and of a counter read never panic (inputs of
a few bytes; longer ones are left to proptest).

Makefile targets for the special cases: `installcheck-counted` fails
if a suite matched the "cannot count" output, and with
`PG_PMU_REQUIRE_COUNTERS=1` the crate's tests fail where they cannot
count; `installcheck-paranoid` and `installcheck-vm` run the suites of
those systems; on a machine that counts, `make` and `make install`
with `PG_PMU_TEST=workers-cannot-count`, then
`installcheck-workers-cannot-count`, run the failing-worker suite
(build and install again without `PG_PMU_TEST` afterwards);
`PG_PMU_COUNTERS=none` builds a module that cannot count.

**CI** builds PostgreSQL 19 and master on Linux x86-64, Linux AArch64
and macOS. Where the kernel shows a hardware PMU (always on AArch64,
on some x86-64 hosts) CI demands real counts and also runs the
paranoid and failing-worker suites. macOS runners are virtual
machines and run the VM suite. Every job also runs the suites of a
build without counters (`PG_PMU_COUNTERS=none`). A separate workflow
runs the observations of [observations.md](observations.md) on Linux.

## Files

```
 pg_pmu.c          the option, the hooks, frames, records, output
 workers.c         shared memory, pg_pmu.cell, holds, worker counting
 pg_pmu.h          what the two C files share
 pmu_counters.h    C declarations of the crate's functions
 test/abi.c        layout checks against the crate and the kernel
 crates/pmu_counters/
   src/cells.rs    the cells: layout, token, steps, Kani proofs
   src/counters/   Linux perf events, macOS rusage, references
   src/sysfs.rs    core types and their CPUs
   src/ffi.rs      the functions C calls
   tests/          proptest models, loom models, real counters
 sql/, expected/   regression suites
 README.md         goals, limitations, use, build
 doc/observations.md  how stable the counts are, measured
 .github/          CI, observations, PostgreSQL build
```
