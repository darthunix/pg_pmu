# pg_pmu

`pg_pmu` adds the option `PMU` to `EXPLAIN ANALYZE`. With it, EXPLAIN
shows how many instructions and cycles of the processor one statement
took, in the backend and in its parallel workers. It is meant for
comparing two ways to run the same query by the work they do.

It supports PostgreSQL 19 and master, on Linux and macOS.

## Building

The module is C, with a Rust crate, `crates/pg_pmu_core`, linked in as
a static library, so building needs a Rust toolchain besides the
server's headers:

```sh
make PG_CONFIG=/path/to/pg_config
make install PG_CONFIG=/path/to/pg_config
```

Cargo fetches the crate's dependencies from crates.io, at the versions
of `Cargo.lock`. To build without the network, run `cargo vendor` in
`crates/pg_pmu_core` beforehand and point Cargo at the sources it
writes, as it prints. Cargo runs only when a source of the crate has
changed, so `make install` after `make` runs none. `make
PG_PMU_COUNTERS=none` builds a version that cannot count, as on any
system other than Linux and macOS; give `make install` the same
setting. `make clean` keeps the crate's target directory; `cargo clean`
in the crate removes it.

The crate never panics on purpose: a panic would abort the process of
PostgreSQL, and the server would restart every session.

## Loading

The module must be loaded when the server starts:

```
shared_preload_libraries = 'pg_pmu'
```

It has no SQL objects, so it needs no `CREATE EXTENSION`. Loaded in
any other way, as with `LOAD` or `session_preload_libraries` on a
server that does not preload it, it fails with SQLSTATE 55000. With
`session_preload_libraries` that failure refuses every new connection
that has the setting.

## Settings

- `pg_pmu.max_nested` (16, from 0 to 1024, set before the start): the
  number of nested counting statements, an `EXPLAIN (ANALYZE, PMU)`
  that starts while another one of the same backend runs, that the
  whole server can run at once. A nested statement that finds all of
  them in use fails with SQLSTATE 53000 before it runs. Each costs 32
  bytes of shared memory; the module takes 16 + 32 * (N + K) bytes in
  all, N being the number of backends the server can run and K this
  setting.

The module also has a setting `pg_pmu.cell` for its own use, hidden,
which tells the parallel workers of a statement where to report. A
session cannot change it.

## Use

```sql
EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, BUFFERS off)
  SELECT count(*) FROM t;
```

```
 Aggregate (actual rows=1.00 loops=1)
   ->  Seq Scan on t (actual rows=100000.00 loops=1)
 Planning Time: 0.349 ms
 Planning PMU: instructions=2165997 cycles=1212815
 Execution PMU: instructions=49678384 cycles=10563856
 Execution Time: 3.769 ms
```

The planning is counted when this EXPLAIN planned the statement; a
plan from the cache of prepared statements, as with `EXPLAIN EXECUTE`,
has no counts of planning, even when the cache makes it during the
EXPLAIN. In JSON and YAML the counts are the properties "Planning
Instructions", "Planning Cycles", "Execution Instructions" and
"Execution Cycles" of the query, in XML the elements with the spaces
replaced by hyphens, `Planning-Instructions` and so on. They come
after the plan, the triggers, JIT and serialization, and before
"Execution Time", also with `SUMMARY off`. Each plan of an EXPLAIN has
its own, as with a rule that makes two.

`PMU` takes the same values as PostgreSQL's own boolean options, and
needs `ANALYZE`: without it EXPLAIN fails with SQLSTATE 22023.
auto_explain cannot use it, as it explains a plan after it has run.

### What the counts include

Instructions are those the processor completed, cycles the clock
cycles of the cores while they ran them. The execution counts cover
the start of the executor, its run and the AFTER triggers, with every
function and query the statement runs inside. They leave out the end
of the executor, where it frees its resources, which comes after the
counts are printed.

They include the work EXPLAIN ANALYZE does to measure each node of
the plan, a few dozen instructions for each row of each node; it
weighs more on an executor that passes rows one by one than on one
that passes them in batches. `TIMING off` makes it smaller.

On Linux the counts are of the user space of the backend's thread; on
macOS of the whole process, kernel included. Counts of the two
systems are not comparable.

### Parallel workers

The parallel workers of a statement count their own work and add it
to the counts of the execution: each counts its execution of the
plan, with the queries it runs inside. With `VERBOSE` the output also
shows their part, when one of them counted; here two workers scanned
less than a tenth of the table, the backend the rest:

```
 Execution PMU: instructions=52818896 cycles=9396448
 Workers PMU: instructions=7248083 cycles=1922555
```

in JSON and YAML "Workers Instructions" and "Workers Cycles". It is
shown also when EXPLAIN hides the Gather, as with
`debug_parallel_query` set to `regress`. A counting EXPLAIN that runs
inside another one counts the workers of its own plans, and the outer
one includes all of its work, its workers too, also when it fails and
a function catches the error. The workers of a statement that does
not count do not count, and an EXPLAIN that a worker runs has no part
of the workers.

A worker that cannot count fails with the same error as a backend
that cannot, and so does the statement. When a function catches that
error, the counting statement around it still fails at its end, with
the same SQLSTATE, rather than show counts without the worker. An
ordinary error of a worker is not such a failure: what the worker did
until then is counted.

### When the system cannot count

When the system cannot count, a counting EXPLAIN fails before the
statement runs: with SQLSTATE 42501 when the system does not allow
it, 0A000 when it has no counters for the process, as in a virtual
machine, and 53000 when no hardware counter or file descriptor is
free.

## Observations

How much counts vary from run to run, and an example with
[Tessera](https://github.com/darthunix/tessera), a batch executor for
PostgreSQL. On an Apple M5 Pro with 64 GB, macOS 26.6.2, the machine
otherwise idle; PostgreSQL master at 311df1dc, built without
assertions; Tessera at 2cea4cea, a release build; both modules
preloaded, pg_pmu last, so that its hooks wrap Tessera's:

```
shared_preload_libraries = 'tessera, tessera_nodes, tessera_kernels, pg_pmu'
```

```sql
CREATE EXTENSION tessera;
CREATE TABLE t AS SELECT g FROM generate_series(1, 100000) g;
VACUUM ANALYZE t;
```

Each statement ran eleven times in one session, and the first run,
which fills the caches of the backend, is left out:

```sh
run() {
  for i in $(seq 11); do echo "$1"; done |
    psql -XAt | grep 'Execution PMU' | tail -10
}
run "EXPLAIN (ANALYZE, PMU, TIMING off) SELECT 1;"
run "EXPLAIN (ANALYZE, PMU, TIMING off) SELECT count(*) FROM t;"
run "SET tessera.enable = off;
     EXPLAIN (ANALYZE, PMU, TIMING off) SELECT count(*) FROM t;"
```

The counts of the execution, the smallest of the ten and, in
brackets, how much the largest exceeds it:

- `SELECT 1`: 10 837 instructions (0.23 %), 3 482 cycles (165 %);
- `count(*)` with Tessera: 5 469 677 instructions (0.0003 %),
  956 601 cycles (5.8 %);
- `count(*)` with PostgreSQL's own plan: 43 152 878 instructions
  (0.02 %), 7 596 853 cycles (0.9 %).

The same commands on a runner of GitHub's CI with AArch64 Linux, a
virtual machine with four Neoverse N2 cores and Linux 7.0, shared with
other machines; the workflow `Observations` builds and runs them
(`.github/workflows/observations.yml`):

- `SELECT 1`: 4 592 instructions (0 %), 4 301 cycles (325 %);
- `count(*)` with Tessera: 6 070 572 instructions (0 %),
  1 865 269 cycles (13.7 %);
- `count(*)` with PostgreSQL's own plan: 43 310 950 instructions
  (0 %), 13 584 830 cycles (1.4 %).

Instructions repeat to a few in a hundred thousand on the Mac and
exactly on Linux, and so compare two plans of one query run by run.
Cycles depend on the caches and on the clock of the core, and vary
much more, most for short statements and on a shared machine. Tessera's
plan of the `count(*)` ran 7.9 times fewer instructions and cycles
than PostgreSQL's own on the Mac, and 7.1 times fewer instructions and
7.3 times fewer cycles on Linux. On macOS the counts include the
kernel's work for the process, on Linux they do not, which is why
`SELECT 1` counts fewer instructions there.

An Intel Mac, a Core i5-1038NG7 with macOS 26.5, counts too: the
suites pass there with their first expected outputs.

## Testing

The crate has its own checks, with Clippy and a look for panics in
the archive the module links:

```sh
make check-crate PG_CONFIG=/path/to/pg_config
```

The suites of SQL start temporary servers of their own:

```sh
make installcheck PG_CONFIG=/path/to/pg_config \
    EXTRA_REGRESS_OPTS="--temp-instance=$PWD/tmp_check"
```

The suite `option` uses auto_explain, so the server's `contrib` must
be installed. On a system that cannot count, the suites match their
second expected outputs, where each counting EXPLAIN prints "cannot
count". On a machine that must count, `make installcheck-counted`
fails if a suite matched one of those, and `PG_PMU_REQUIRE_COUNTERS=1`
makes the crate's tests fail where they cannot count.

A worker that cannot count while its backend can is shown by a build
whose parallel workers fail to open their counters, on a machine that
counts:

```sh
make PG_PMU_TEST=workers-cannot-count PG_CONFIG=/path/to/pg_config
make PG_PMU_TEST=workers-cannot-count install PG_CONFIG=/path/to/pg_config
make installcheck-workers-cannot-count PG_CONFIG=/path/to/pg_config \
    EXTRA_REGRESS_OPTS="--temp-instance=$PWD/tmp_check"
```

Build the module again without `PG_PMU_TEST` afterwards: that build is
for this suite only.

## License

The PostgreSQL License; see [LICENSE](LICENSE).
