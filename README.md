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
two plans, two settings, two executors. On Linux it also counts, when
asked, the branches and those the processor mispredicted, and the
references to the cache and its misses, which help explain why one
way takes more cycles than another:

```
EXPLAIN (ANALYZE, PMU branches, TIMING off) SELECT count(*) FROM t;
 ...
 Execution PMU: instructions=43218494 cycles=7733504 branches=7915310 branch-misses=1201
```

How the module works is in [doc/design.md](doc/design.md); on
Greengage, see [gg/README.md](gg/README.md).

## Goals

- Counts for one statement, parallel workers included, in EXPLAIN's
  output.
- A small cost: a few reads of the counters per statement and
  process, whatever the number of rows. No counter is read without the
  option; while an EXPLAIN runs, the hooks add a little bookkeeping to
  each query it runs, which a counting statement counts too.
- Nothing to create in the database, and shared memory of a size fixed
  at start, about 10 KB with default settings.
- A count is right or the statement fails. Never a plausible wrong
  number.

Not goals: counts per plan node, statistics over many statements (see
pg_stat_kcache for that), systems other than Linux and macOS.

## Limitations

- PostgreSQL 19 and master; Linux and macOS. Greengage 7 with the
  hooks of EXPLAIN for extensions, see [gg/README.md](gg/README.md).
- On PostgreSQL, loaded only with `shared_preload_libraries`, so it
  needs a restart.
- Linux needs `kernel.perf_event_paranoid` of 2 or less, or
  `CAP_PERFMON`. Containers and security modules may refuse too.
- Virtual machines often have no counters, among them GitHub's macOS
  runners and some of its x86-64 Linux runners. Some count
  instructions and the branches but hide the cache, among them other
  x86-64 runners of GitHub; there `PMU cache` fails with 0A000.
- Both count every thread of the process. Linux counts user space
  only, and also the processes the backend starts once it counts,
  such as the program of `COPY ... PROGRAM`; before Linux 4.13 it
  misses threads started after its first counting statement. macOS
  counts the kernel too. Do not compare the two.
- The counts include EXPLAIN ANALYZE's own measuring of each node, a
  few dozen instructions per row per node. `TIMING off` reduces it.
- The branches and the cache are counted on Linux only: macOS lets
  only root count them. They are the kernel's generic events, so the
  level of the cache they count depends on the processor: the last
  level on Intel, the second on AMD, the first on Arm. Compare them on
  one machine only. They change from run to run more than
  instructions do: they explain a difference rather than decide it.
- Each pair of events takes two hardware counters of a core while a
  statement counts it, and a core has few: `branches` or `cache` needs
  four free, `full` six. AMD processors have six and Intel ones with
  hyper-threading four besides fixed ones, and the NMI watchdog of
  Linux holds one, so there `full` can fail with 53000; ask for the
  branches and the cache in two statements. A nested statement that
  asks for another pair than the statement around it needs both: its
  parallel workers count the pairs of both, and in the backend it may
  take the counters of the statement around, which then fails with
  53000.
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
EXPLAIN (ANALYZE, PMU branches) SELECT ...; -- and the branches
EXPLAIN (ANALYZE, PMU cache) SELECT ...;    -- and the cache
EXPLAIN (ANALYZE, PMU full) SELECT ...;     -- and both
```

- `PMU` needs `ANALYZE`. The counts are printed after the plan.
- `PMU` alone, or with a Boolean value, counts instructions and cycles.
  `branches` adds `branches` and `branch-misses`, `cache` adds
  `cache-references` and `cache-misses`, and `full` both. As with
  PostgreSQL's own options, the last value given counts.
- It works only in an `EXPLAIN` statement: auto_explain, whose plans
  have already run, and code that explains plans on its own cannot use
  it.
- JSON and YAML have the properties "Execution Instructions",
  "Execution Cycles", "Execution Branches", "Execution Branch Misses",
  "Execution Cache References", "Execution Cache Misses", "Planning
  ..." and "Workers ...". XML has the same names with hyphens.
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
- 0A000: there are no counters, as in many virtual machines, the
  kinds of cores of the processor cannot all be counted, the process
  runs more than 64 threads, or the system cannot count the branches
  or the cache asked for, as macOS;
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
that cannot count they match their second expected outputs; the
suites `events_branches` and `events_cache` have three: a system that
counts the pair, one that counts only instructions and cycles, as
macOS, and one that counts nothing. For the cases that need a machine
of their own:

- `make installcheck-counted` fails if the suites did not count, and
  `PG_PMU_REQUIRE_COUNTERS=1` makes the crate's tests fail the same
  way; with `PG_PMU_REQUIRE_EVENTS=1` both demand the branches and the
  cache too, the six events at once;
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
