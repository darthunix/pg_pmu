# Tasks

Each task is one commit. "The suite" means

```sh
make && make install &&
make installcheck EXTRA_REGRESS_OPTS="--temp-instance=$PWD/tmp_check"
```

with `PG_CONFIG` of a PostgreSQL 19 build and then of a master build,
both with assertions. The `Makefile` runs the regression suite twice:
`sql/unloaded.sql` on a temporary server without the module, and the
other suites on one started with `pg_pmu.conf`. "The checks of the
crate" mean, in `crates/pg_pmu_core`, `cargo test`, `cargo clippy`
with the lints of the design, and the commands each task names.

## 1. The module and its option

- [x] 1.1 Add the PGXS `Makefile` with its two runs, `pg_pmu.conf`,
  and `pg_pmu.c`: the refusal of any loading but
  `shared_preload_libraries` (55000), the setting `pg_pmu.max_nested`,
  the option `PMU`, the check that it comes with `ANALYZE` (22023), the
  check that refuses it in auto_explain. Add a README with how to
  build and load the module, `sql/unloaded.sql` (not loaded, 42601;
  `LOAD`, 55000) and `sql/option.sql`, which needs no counters: without
  `ANALYZE`, turned off, auto_explain. Verify with the suite on both
  builds.

## 2. The crate and the counters of one process

- [x] 2.1 Add the crate `crates/pg_pmu_core`: `no_std` except under
  its tests, a static library and a library, `libc` without its
  default features, `Cargo.lock`, the lints against panics, and its
  build in the `Makefile`, with the feature `none` behind
  `PG_PMU_COUNTERS=none` and on any target other than Linux and macOS.
  Add `pg_pmu_core.h` and `test/abi.c`, which includes and calls it.
  In the crate: the counters of macOS and of `none`, the references
  that enable and disable them, the codes. In C: open and check the
  counters when EXPLAIN has read its options, and turn the codes into
  the SQLSTATEs of the spec. Add the tests of the crate on real
  counters in `tests/counters.rs`, and the suite `sql/counters.sql`,
  with the function that prints "cannot count", its second expected
  output, and the case "Nothing runs". Verify with the checks of the
  crate and the suite on the Mac, and again with the suite built with
  `PG_PMU_COUNTERS=none` in each of its three commands, which must
  match the second output.
- [ ] 2.2 Add the counters of Linux to the crate:
  `src/counters/perf_abi.rs` with the declarations of perf events,
  compared in `test/abi.c` with `<linux/perf_event.h>` when the module
  loads; the kinds of cores
  from the sysfs root it is given, and their coverage of the online
  processors; the pinned groups opened disabled, with neither kernel
  nor hypervisor; the checks of each read; the number of descriptors,
  which C takes from the budget of external files and gives back when
  an opening fails, while the crate closes what it opened; the error
  of the system for the hint. Add the tests of proptest over random
  sysfs directories and random sequences of references against their
  models, a test of an opening that fails on its second group and of
  the next one that succeeds, and the proofs of Kani for the parser of
  a list of processors and the check of a read within the bounds of
  the design. Verify with the checks of the crate and `cargo kani`,
  with the suite on Linux on a machine with counters, and by hand
  there: with `kernel.perf_event_paranoid` set to 4 on an Ubuntu
  kernel (42501), with all counters taken by a pinned system-wide
  `perf stat` run as root (53000), and that the next statement counts
  again.

## 3. Counting the statement

- [x] 3.1 Count the execution in the backend: the frames in the hook
  of utility statements, the depth, the records of the candidates and
  their removal in the hook of the end of the executor, the reads at
  the start and the finish of the executor, the choice of the record
  of the printed plan whose query has not ended, the output in all
  formats. Add to the suite: "Loaded at the start", "With ANALYZE",
  text with numbers replaced, JSON, YAML and XML with the properties
  of the execution, `SUMMARY off`, more rows more work, the work of a
  function, a rule with two plans, a rule with no plan, an EXPLAIN
  followed by other statements in one call of SPI, a parameter of
  `EXPLAIN EXECUTE` that opens a cursor, an EXPLAIN that fails inside
  an `EXCEPTION` block and one that counts after it. Add the use and
  what the counts include to the README. Verify with the suite on both
  builds.
- [x] 3.2 Count the planning in the hook of the planner, and show it
  only for a plan made in that EXPLAIN. Add to the suite a query
  planned by EXPLAIN, a prepared statement explained twice, and the
  properties of the planning in every format. Verify with the suite
  on both builds.

## 4. The cells in Rust

- [x] 4.1 Add `src/cells.rs` to the crate: the header and the cells
  as `#[repr(C)]` types with tests of their sizes and offsets against
  the spec, the 64-bit token with its decimal form and its checks of
  validity, the steps (take, enter, add, leave, close, empty, open,
  read) over an atomic type that `cfg(loom)` switches, all
  sequentially consistent, the generation wrapping modulo 2^44, the
  bound of the writers, the codes, the refusal to build without 64-bit
  atomics, and the tests of proptest in `tests/model.rs` against a
  plain model, with the bounds of the token and of the generation.
  Verify with the checks of the crate.
- [x] 4.2 Add the models of loom in `tests/loom.rs`, two or three
  threads each: reports against a close; a late report with an old
  token against a close and a new taking; two backends racing for one
  cell of the pool; a nested cell handed down into the hold below
  while a report comes. Verify with
  `RUSTFLAGS="--cfg loom" cargo test --release --test loom` in
  `crates/pg_pmu_core`.
- [x] 4.3 Add the proofs of Kani for the cells, within the domains of
  the design: the token and its decimal form made and read back, only
  valid tokens accepted, every step from an allowed state to an
  allowed one or a refusal, no panic. Verify with `cargo kani` in
  `crates/pg_pmu_core`.

## 5. The parallel workers

- [x] 5.1 Add `workers.c`: the registration of the shared memory right
  after the check of initialization, handing its address, N and K to
  the crate; the setting `pg_pmu.cell` of the start of a connection, a
  string with the token, with its check and flags, saved by the frames
  in a nesting level of the settings and checked at each finish; the
  cell of the outermost frame, a cell of the pool for a nested one
  (53000 when none is free), the holds of cells apart from the frames,
  each pointing to the hold below, their return at the end or from the
  callback of the abort of their own subtransaction, innermost first,
  with the whole cell handed to the hold below. Add to the suite: the
  size in `pg_shmem_allocations`, the attempts to change
  `pg_pmu.cell`, a third nested statement at once, a nested one that
  fails inside two nested `EXCEPTION` blocks and its cell taken again
  after them. Verify with the suite on both builds.
- [x] 5.2 Count in the workers: their execution of the plan by its
  tuple queue, the token checked when they start to count, the report
  through the crate at the finish or from the exit callback, the count
  of failures, the refusal of an old or not valid token (XX000), the
  reference count of the counters of a process, the differences of
  the cell in the records, the part of the workers with `VERBOSE`.
  Add to the suite the other cases of the workers in the design's
  Tests, two nested statements that fail together among them.
  Describe the workers and `pg_pmu.max_nested` in the README. Verify
  with the suite on both builds, and by hand: a worker that cannot
  count under a function that catches its error.

## 6. CI

- [x] 6.1 Add `.github/workflows/ci.yml`: build PostgreSQL 19 and
  master at fixed revisions, with assertions and with auto_explain
  from contrib, on Ubuntu and on macOS on Apple silicon; on Ubuntu set
  `kernel.perf_event_paranoid` to 2 with `sudo sysctl`; run the suite;
  print which expected output each suite matched; run the checks of
  the crate, the models of loom and `cargo kani`. Verify with a green
  run, and read in its log whether the runners could count.

## 7. Checks of the whole

- [ ] 7.1 On the Mac and on a Linux machine with counters, after the
  owner confirms the machine is idle, run each of these ten times:
  `EXPLAIN (ANALYZE, PMU, TIMING off)` of `SELECT 1`, of a `count(*)`
  over 100000 rows, and of the same `count(*)` with Tessera on and
  off. On the Intel Mac run the suite once. Write the counts as
  observations into the README, with the machine, the conditions and
  the commands: the smallest, the spread from run to run, and the
  example with Tessera. Verify that the README's commands run as
  written and give positive counts.
- [ ] 7.2 Close the change: replace each "Verified by: pending" with
  the test that shows it. Write "review only" with the reason only for
  a property no test can show, such as a refusal of a kernel the CI
  cannot make; a case that a test could show but no machine of CI can
  run, such as "A caught failure of a worker", names its test and says
  where it runs. Verify with
  `openspec validate add-explain-pmu --strict`.
