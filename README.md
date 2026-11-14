# pg_pmu

pg_pmu is a PostgreSQL extension that adds the option `PMU` to
`EXPLAIN ANALYZE`. It shows how many instructions and cycles the
processor spent on one statement, its parallel workers included:

```
EXPLAIN (ANALYZE, PMU, TIMING off) SELECT count(*) FROM t;
 ...
 Planning PMU: instructions=30022 cycles=31216
 Execution PMU: instructions=43218494 cycles=7733504
```

Time changes from run to run; the number of instructions barely does.
That makes pg_pmu a tool for comparing two ways to run the same query:
two plans, two settings, two executors. How the module works is in
[doc/design.md](doc/design.md).

## Goals

- Counts for one statement, parallel workers included, in EXPLAIN's
  output.
- A small cost: a few reads of the counters per statement and
  process, whatever the number of rows. No counter is read without the
  option; while an EXPLAIN runs, the hooks add a little bookkeeping to
  each query it runs, which a counting statement counts too.
- Nothing to create in the database, and shared memory of a size fixed
  at start, about 5 KB with default settings.
- A count is right or the statement fails. Never a plausible wrong
  number.

Not goals: counts per plan node, statistics over many statements (see
pg_stat_kcache for that), systems other than Linux and macOS.

## Limitations

- PostgreSQL 19 and master; Linux and macOS.
- Loaded only with `shared_preload_libraries`, so it needs a restart.
- Linux needs `kernel.perf_event_paranoid` of 2 or less, or
  `CAP_PERFMON`. Containers and security modules may refuse too.
- Virtual machines often have no counters, among them GitHub's macOS
  runners and some of its x86-64 Linux runners.
- Linux counts user space of the process's thread only; macOS counts
  the whole process, kernel included. Do not compare the two.
- The counts include EXPLAIN ANALYZE's own measuring of each node, a
  few dozen instructions per row per node. `TIMING off` reduces it.
- Not counted: the executor's end, the start of the workers, and
  workers that run no plan, such as those of a parallel index build.

## Use

Add the module to `postgresql.conf` and restart:

```
shared_preload_libraries = 'pg_pmu'
```

There is no `CREATE EXTENSION`. Then:

```sql
EXPLAIN (ANALYZE, PMU) SELECT 1;            -- planning and execution
EXPLAIN (ANALYZE, PMU, VERBOSE) SELECT ...; -- with "Workers PMU" when
                                            -- parallel workers counted
EXPLAIN (ANALYZE, PMU, FORMAT JSON) SELECT ...;
```

- `PMU` needs `ANALYZE`. The counts are printed after the plan.
- It works only in an `EXPLAIN` statement: auto_explain, whose plans
  have already run, and code that explains plans on its own cannot use
  it.
- JSON and YAML have the properties "Execution Instructions",
  "Execution Cycles", "Planning ..." and "Workers ...". XML has the
  same names with hyphens.
- The workers' part is included in the execution counts. `VERBOSE`
  shows it separately when a worker counted.
- Plans from the cache of prepared statements, as with
  `EXPLAIN EXECUTE`, have no planning counts.

Setting:

- `pg_pmu.max_nested` (default 16, set before the start): how many
  counting EXPLAINs nested inside others the whole server can run at
  once. One more fails with SQLSTATE 53000.

When the system cannot count, the statement fails, usually before it
runs:

- 42501: counting is not allowed (see Limitations);
- 0A000: there are no counters, as in many virtual machines, or the
  kinds of cores of the processor cannot all be counted;
- 53000: no free hardware counter or file descriptor, or a counter
  lost while the statement ran.

A parallel worker that cannot count, or a counter lost on the way,
fails the statement later, with the same codes.

## Build

You need a PostgreSQL build with `pg_config` and its headers, a C
compiler and a Rust toolchain (stable).

```sh
make PG_CONFIG=/path/to/pg_config
make install PG_CONFIG=/path/to/pg_config
```

Cargo downloads the crate's dependencies from crates.io. To build
offline, vendor them first, from the repository's root:

```sh
mkdir -p .cargo
cargo vendor --manifest-path crates/pmu_counters/Cargo.toml \
    crates/pmu_counters/vendor > .cargo/config.toml
```

Tests:

```sh
make check-crate PG_CONFIG=/path/to/pg_config
make installcheck PG_CONFIG=/path/to/pg_config \
    EXTRA_REGRESS_OPTS="--temp-instance=$PWD/tmp_check"
```

The suites need auto_explain from the server's `contrib`. On a system
that cannot count they match their second expected outputs. For the
cases that need a machine of their own:

- `make installcheck-counted` fails if the suites did not count, and
  `PG_PMU_REQUIRE_COUNTERS=1` makes the crate's tests fail the same way;
- `make installcheck-paranoid`: Linux with `kernel.perf_event_paranoid`
  set to 4;
- `make installcheck-vm`: a macOS virtual machine;
- `make installcheck-workers-cannot-count`: on a machine that counts,
  after `make` and `make install` with
  `PG_PMU_TEST=workers-cannot-count`, whose workers fail to open their
  counters; build and install again without it afterwards;
- `PG_PMU_COUNTERS=none` builds a module that cannot count.

Give `make install` the same `PG_PMU_COUNTERS` and `PG_PMU_TEST` as
`make`; otherwise it stops.

## License

The PostgreSQL License; see [LICENSE](LICENSE).
