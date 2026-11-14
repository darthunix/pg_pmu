# Observations

How much the counts of pg_pmu vary from run to run, and an example of
comparing two executors of one query, PostgreSQL's own and
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
