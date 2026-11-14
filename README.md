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
  whole server can run at once.

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
replaced by hyphens, `Planning-Instructions` and so on. They come after the
plan, the triggers, JIT and serialization, and before "Execution
Time", also with `SUMMARY off`. Each plan of an EXPLAIN has its own,
as with a rule that makes two.

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

When the system cannot count, a counting EXPLAIN fails before the
statement runs: with SQLSTATE 42501 when the system does not allow
it, 0A000 when it has no counters for the process, as in a virtual
machine, and 53000 when no hardware counter or file descriptor is
free.

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

## License

The PostgreSQL License; see [LICENSE](LICENSE).
