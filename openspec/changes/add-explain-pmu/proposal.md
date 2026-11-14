# Proposal

## Why

To compare two executors of one query, such as PostgreSQL's own and
Tessera's, we want to know how much work each of them did. Time is a
poor measure of work: it changes from run to run with the frequency of
the processor, the other processes and the state of the caches. The
number of instructions the processor executed changes much less, and
the number of cycles beside it tells how fast they ran.

PostgreSQL cannot show these numbers for one statement today.

- `EXPLAIN ANALYZE` shows times, rows and buffers, not instructions.
- pg_stat_kcache keeps the CPU time of each normalized query in a table
  in shared memory, summed over all its runs. It has no instructions,
  and it does not tell one run from another.
- `perf stat -p <pid>` counts a whole backend over a period of time,
  not one statement. It misses the parallel workers, which are other
  processes.

The case: `EXPLAIN ANALYZE SELECT count(*) FROM t` with Tessera on and
off gives two times that differ by a few percent from run to run. There
is no way to ask how many instructions each run took, with the
parallel workers included.

## What Changes

- A new module, `pg_pmu`, loaded with `shared_preload_libraries`. It
  has no SQL objects, so it needs no `CREATE EXTENSION`.
- A new option of EXPLAIN, `PMU`, allowed only with `ANALYZE`.
- With it, EXPLAIN shows the instructions and cycles of the
  statement's execution, the backend and its parallel workers
  together, and of its planning when this EXPLAIN planned it. With
  `VERBOSE` it also shows the part done by the parallel workers.
- On Linux it counts with the kernel's performance events, in user
  space only. On macOS it counts with the counters the kernel keeps for
  each process, user space and kernel together.
- When the backend cannot count, the statement fails with an error and
  its SQLSTATE before it runs; a worker that cannot count fails it
  later.
- PostgreSQL 19 and master.

## Capabilities

### New Capabilities

- `explain-pmu`: the EXPLAIN option `PMU`, its output in every format,
  which work and which processes the counts cover on each system, the
  errors when the system cannot count, and the shared memory the
  parallel workers report to.

### Modified Capabilities

None.

## Impact

- A new repository: a PGXS `Makefile`, the C sources, a Rust crate for
  the cells of the shared memory and the counters of the system with
  its checks of loom, proptest and Kani, an SQL regression suite, a
  README and CI on Linux and macOS against PostgreSQL 19 and master.
  Building needs a Rust toolchain.
- No SQL objects. One setting of the start, `pg_pmu.max_nested`, the
  number of nested counting statements the server runs at once, and
  one hidden setting the module keeps for its workers.
- Shared memory of a fixed size, requested at the start of the server:
  32 bytes for each backend and for each cell of the pool of nested
  statements, about 4.8 KB with the defaults.
- What the system must allow: on Linux, usually
  `kernel.perf_event_paranoid` of 2 or less, or the capability
  `CAP_PERFMON`, and on processors with two kinds of cores a kernel
  that can count each kind (on Intel, Linux 5.13 or later); on macOS,
  a kernel that keeps the counters of processes, as on Apple silicon
  outside a virtual machine.
