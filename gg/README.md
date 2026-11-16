# pg_pmu on Greengage

The same option `PMU` of `EXPLAIN ANALYZE` on Greengage 7, a database
of many segments. The coordinator counts its own part of a statement.
Each executor of a slice on a segment (a QE) counts its part and sends
the counts to the coordinator with the statistics of its slice, and
the coordinator adds them in:

```
EXPLAIN (ANALYZE, PMU, VERBOSE, TIMING off, SUMMARY off)
SELECT b, count(*) FROM t GROUP BY b;
 ...
 Planning PMU: instructions=152356 cycles=141046
 Execution PMU: instructions=143337101 cycles=29489701
 Coordinator PMU: instructions=2454633 cycles=2091905
 Slice 1 PMU: instructions=1638450 cycles=1550081 QEs=3, max instructions=581718 (seg0)
 Slice 2 PMU: instructions=139244018 cycles=25847715 QEs=3, max instructions=46557250 (seg0)
```

- `Execution PMU` is the work of the whole statement: the coordinator
  and every QE.
- Under `VERBOSE`, `Coordinator PMU` is the coordinator's part, and
  each slice that ran on the segments has a line: the counts of its
  QEs together, their number, and the most instructions of one QE with
  its segment. A slice of an initplan has its line too.
- The QEs of an EXPLAIN that a function runs inside the statement,
  with PMU or with ANALYZE alone, are counted in `Execution PMU`, and
  under `VERBOSE` shown as `Nested QEs PMU`; those of one the planner
  runs, in `Planning PMU`.
- JSON and YAML have the properties "Coordinator Instructions",
  "Coordinator Cycles", "Nested QEs Instructions" and "Nested QEs
  Cycles", and a list "Slices PMU", beside those of PostgreSQL.

## Needs

- Greengage with the hooks of EXPLAIN for extensions: the options and
  the hooks for each plan and node of PostgreSQL 18
  (`commands/explain_state.h`), and the hooks with which a QE sends
  its own data to the coordinator (`cdbexplain_send_hook`,
  `cdbexplain_recv_hook`).
- The module in every backend of the cluster, the QEs' included, set
  with `gpconfig`:

  ```
  gpconfig -c session_preload_libraries -v pg_pmu
  gpstop -u
  ```

  or `shared_preload_libraries` and a restart. A statement fails rather
  than show counts without those of a QE that sent its statistics
  without them.
- `PMU` works on the coordinator, not in utility mode.
- `PMU` takes a Boolean value only: the branches and the cache, which
  `PMU branches`, `cache` and `full` count on PostgreSQL, are not
  counted on Greengage, whose QEs count instructions and cycles.

## Limitations

- Each QE counts every `EXPLAIN ANALYZE`, with or without the option:
  it cannot know which one asked. That costs, per slice, a reference
  to the counters taken and given back and two reads. A QE that the
  system does not let count tries once a session; one short of a
  counter or a descriptor tries again at the next slice.
- When a QE cannot count, a statement with the option fails with the
  code of the QE's reason (see the codes in the main README); others
  go on.
- A query that a function runs on the segments inside the statement,
  outside an EXPLAIN ANALYZE, sends no statistics, so no counts: the
  statement fails. So does a fetch inside it from a cursor on the
  segments opened before, unless the cursor is at its end: the
  coordinator cannot tell when the cursor's QEs are done. So does a
  statement inside which an EXPLAIN failed, even when a function
  caught the error, since the counts of its QEs are lost. A query that
  auto_explain analyzes counts as one outside, as auto_explain may
  never take its statistics in.
- Not seen, and so not counted without an error: the work on the
  segments of a utility statement that a function runs inside the
  statement, such as CREATE TABLE or ANALYZE, which the core sends to
  the segments outside the executor; and the arguments of `EXPLAIN
  EXECUTE`, computed before the plan runs, on the coordinator and on
  the segments alike.
- Not counted: what a QE does before its executor starts (reading the
  plan, its transaction) and its executor's end; the QEs of a cursor
  opened before, which work on their own from its DECLARE, when a
  function only closes it inside the statement; ic-proxy, a process
  that all sessions share.
- Instructions add up across hosts; cycles only roughly, as hosts may
  run at other speeds.
- The planning in the cache of prepared statements, or in another
  extension's `ExplainOneQuery_hook`, is not counted.

## Build and tests

`make` builds the directory of the core that `pg_config` names, so
build with the `pg_config` of Greengage:

```sh
make PG_CONFIG=$GPHOME/bin/pg_config
make PG_CONFIG=$GPHOME/bin/pg_config install
```

The suites run against a running cluster of three segments, such as
the demo cluster of Greengage, and load the module with `gpconfig`
themselves, so it must not be loaded by `shared_preload_libraries`:

```sh
make PG_CONFIG=$GPHOME/bin/pg_config installcheck
```

On macOS add `EXTRA_REGRESS_OPTS=--use-existing` and create the
database `contrib_regression` first. The suite `qes_cannot_count` needs
the build `PG_PMU_TEST=qes-cannot-count`, whose QEs cannot count:

```sh
make PG_CONFIG=$GPHOME/bin/pg_config PG_PMU_TEST=qes-cannot-count
make PG_CONFIG=$GPHOME/bin/pg_config PG_PMU_TEST=qes-cannot-count install
make PG_CONFIG=$GPHOME/bin/pg_config installcheck-qes-cannot-count
```
