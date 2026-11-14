# Spec Delta

## Purpose

Shows in EXPLAIN ANALYZE how many instructions and cycles of the
processor one statement took, its parallel workers included, so that
two ways to run the same query can be compared by their work.

## ADDED Requirements

### Requirement: Loading the module

`pg_pmu` SHALL be loaded with `shared_preload_libraries`, so adding it
needs a restart of the server. It SHALL need no `CREATE EXTENSION`.
Every process of the server, the parallel workers included, then has
the module. Started any other way, as by `LOAD` or by
`session_preload_libraries` on a server that does not preload it, the
module SHALL fail with SQLSTATE 55000 and a message that names
`shared_preload_libraries`; with `session_preload_libraries` this
fails every new connection that has the setting. A `LOAD` of the
module on a server that preloads it does nothing.

#### Scenario: Loaded at the start

- **WHEN** the server starts with `pg_pmu` in
  `shared_preload_libraries` and a session runs
  `EXPLAIN (ANALYZE, PMU) SELECT 1`
- **THEN** the output has the counts
- **Verified by:** pending

#### Scenario: Loaded later

- **WHEN** a session of a server without `pg_pmu` in
  `shared_preload_libraries` runs `LOAD 'pg_pmu'`
- **THEN** it fails with SQLSTATE 55000
- **Verified by:** pending

#### Scenario: Not loaded

- **WHEN** a session of a server without `pg_pmu` runs
  `EXPLAIN (ANALYZE, PMU) SELECT 1`
- **THEN** PostgreSQL refuses the unknown option with SQLSTATE 42601
- **Verified by:** pending

### Requirement: The option PMU

`PMU` SHALL be a boolean option of EXPLAIN. It SHALL take the same
values as the boolean options of PostgreSQL itself, and it is off
when not given. It SHALL be refused without `ANALYZE`, with SQLSTATE
22023 and the message "EXPLAIN option PMU requires ANALYZE". When it
is off, the output SHALL be the same as without `pg_pmu`. The setting
`auto_explain.log_extension_options` SHALL refuse `pmu`: auto_explain
explains a plan after it has run, when nothing was counted.

#### Scenario: With ANALYZE

- **WHEN** a session runs `EXPLAIN (ANALYZE, PMU) SELECT 1`
- **THEN** the statement runs and the output has the counts
- **Verified by:** pending

#### Scenario: Without ANALYZE

- **WHEN** a session runs `EXPLAIN (PMU) SELECT 1`
- **THEN** the statement fails with SQLSTATE 22023 and does not run
- **Verified by:** pending

#### Scenario: Turned off

- **WHEN** a session runs `EXPLAIN (ANALYZE, PMU off) SELECT 1`
- **THEN** the output has no counts
- **Verified by:** pending

#### Scenario: auto_explain

- **WHEN** a session with auto_explain loaded sets
  `auto_explain.log_extension_options` to `pmu`
- **THEN** the setting is refused with SQLSTATE 22023
- **Verified by:** pending

### Requirement: Counts of the execution

The output SHALL show two numbers for the execution of the statement.
Instructions are the instructions the processor completed; work it
started on a guess and threw away is not counted. Cycles are the clock
cycles of the cores while they ran that work. Both SHALL cover:

- in the backend, the start of the execution, its run and its AFTER
  triggers;
- in each parallel worker that runs a plan for the statement, the
  start of that execution and its run;
- every function and query the statement runs inside, in the backend
  and in the workers;
- the work a worker did before it failed, when the statement goes on
  after the failure, as a function that catches an error does.

They SHALL NOT cover the end of the statement's own execution, in the
backend and in each worker, where the executor frees its resources;
the ends of the queries it runs inside are covered. Nor do they cover
the start of the worker processes, the queries a worker runs outside
its execution of the plan, and workers that run no plan of a query,
such as those of a parallel index build, even when they call functions
that run queries. They include the work EXPLAIN ANALYZE itself does to
measure each node of the plan; with `TIMING off` that work is smaller.

#### Scenario: More rows, more work

- **WHEN** the same `count(*)`, with a sequential scan, no parallel
  workers and no JIT, runs over a table of 1000 rows and over a table
  of 100000 rows
- **THEN** both counts of the larger table are larger
- **Verified by:** pending

#### Scenario: Work of a function

- **WHEN** the statement calls a function that runs a query
- **THEN** the counts include the work of that query
- **Verified by:** pending

### Requirement: Counts of the planning

When the planner made the plan for this EXPLAIN, the output SHALL also
show the instructions and cycles of that planning, in the backend.
When the plan came through the cache of prepared statements, as with
`EXPLAIN EXECUTE`, the output SHALL show no counts of planning. This
holds even when the cache makes or remakes the plan during this
EXPLAIN; that planning is counted nowhere.

#### Scenario: A query planned by EXPLAIN

- **WHEN** a session runs `EXPLAIN (ANALYZE, PMU)` of a `SELECT`
- **THEN** the output has counts of the planning and of the execution
- **Verified by:** pending

#### Scenario: A prepared statement

- **WHEN** a session runs `EXPLAIN (ANALYZE, PMU) EXECUTE` of a
  prepared statement, the first time and again
- **THEN** each output has counts of the execution and none of the
  planning
- **Verified by:** pending

### Requirement: The output

In the text format the counts SHALL be lines of the form
`Planning PMU: instructions=<n> cycles=<n>` and
`Execution PMU: instructions=<n> cycles=<n>`. In the formats JSON and
YAML they SHALL be integer properties of the query: "Planning
Instructions", "Planning Cycles", "Execution Instructions" and
"Execution Cycles". In XML they SHALL be elements of the query with
the same names, the spaces replaced by hyphens, as PostgreSQL does for
all its names: `Planning-Instructions` and so on. They SHALL come after
the plan, the triggers, JIT and serialization, and before "Execution
Time", whatever `SUMMARY` says. When one EXPLAIN shows several plans,
as with a rule, each plan SHALL have its own counts. A statement that
ends with no plan, such as one a rule turns into nothing, SHALL show
no counts.

#### Scenario: Text

- **WHEN** a session runs `EXPLAIN (ANALYZE, PMU) SELECT 1`
- **THEN** the output has one line `Planning PMU:` and one line
  `Execution PMU:`, each with two positive integers
- **Verified by:** pending

#### Scenario: JSON and YAML

- **WHEN** a session runs `EXPLAIN (ANALYZE, PMU)` of `SELECT 1` with
  `FORMAT JSON` and with `FORMAT YAML`
- **THEN** the query has the four properties, each a positive integer
- **Verified by:** pending

#### Scenario: XML

- **WHEN** a session runs `EXPLAIN (ANALYZE, PMU, FORMAT XML)
  SELECT 1`
- **THEN** the query has the four elements, from
  `Planning-Instructions` to `Execution-Cycles`, each a positive
  integer
- **Verified by:** pending

#### Scenario: A rule makes two plans

- **WHEN** a session explains an `INSERT` into a table with a rule
  `ALSO INSERT` into another table
- **THEN** each of the two plans has its own counts
- **Verified by:** pending

#### Scenario: No plan

- **WHEN** a session explains an `INSERT` into a table with a rule
  `DO INSTEAD NOTHING`
- **THEN** the output has no counts
- **Verified by:** pending

### Requirement: The part of the parallel workers

Each parallel worker SHALL count its own execution of a plan once,
with the queries it runs inside, and add it to the counts of the
statement it works for. The workers of a statement that does not count
SHALL NOT count, even while a function they wait on runs a statement
that does. A statement that a parallel worker runs has no workers of
its own and no part of the workers.

With `VERBOSE`, when at least one worker counted, the output SHALL
also show the part of the workers: in the text format the line
`Workers PMU: instructions=<n> cycles=<n>`, in JSON and YAML the
integer properties "Workers Instructions" and "Workers Cycles", in XML
the elements `Workers-Instructions` and `Workers-Cycles`. It is shown
also when EXPLAIN hides the Gather, as with `debug_parallel_query`
set to `regress`. This part is included in the counts of the
execution.

#### Scenario: The workers do the scan

- **WHEN** a session with `parallel_leader_participation` off,
  `min_parallel_table_scan_size` set to 0, the costs of parallel plans
  set to zero and free slots for workers runs
  `EXPLAIN (ANALYZE, PMU, VERBOSE)` of a `count(*)` over a table of
  100000 rows
- **THEN** the plan shows launched workers, and the part of the
  workers is positive and not larger than the counts of the execution
- **Verified by:** pending

#### Scenario: No workers

- **WHEN** a session with `debug_parallel_query` off runs
  `EXPLAIN (ANALYZE, PMU, VERBOSE)` of a plan without parallel workers
- **THEN** the output has no part of the workers
- **Verified by:** pending

#### Scenario: A hidden Gather

- **WHEN** a session with `debug_parallel_query` set to `regress` runs
  `EXPLAIN (ANALYZE, PMU, VERBOSE)` of a `count(*)` that the planner
  can run in a worker
- **THEN** the output has no Gather and has a positive part of the
  workers
- **Verified by:** pending

#### Scenario: In a worker

- **WHEN** a parallel-safe function that a worker runs below a Gather
  runs `EXPLAIN (ANALYZE, PMU, VERBOSE)`
- **THEN** it succeeds and shows no part of the workers
- **Verified by:** pending

### Requirement: Nested counting statements

A counting statement that starts while another counting statement of
the same backend runs is nested. It SHALL count the workers of its own
plans and of the queries it runs, and SHALL NOT count the workers of
the outer statement, also when it starts inside a parallel plan of the
outer one, from a function the backend runs above a Gather. The outer
statement SHALL include all the work of the nested one, its workers
too, also when the nested one fails and a function catches the error,
and when several nested statements fail together and the error is
caught around all of them.

The setting `pg_pmu.max_nested`, an integer from 0 to 1024, 16 by
default, SHALL take effect only at the start of the server. It is the
number of nested statements the whole server can run at once. A
nested statement that finds all of them in use SHALL fail with
SQLSTATE 53000 before it runs.

The module keeps the setting `pg_pmu.cell` for its own use, to tell
the workers where to report: its value is the token of a cell, as the
requirement on the shared memory says, or 0 when the statement does
not count. A session SHALL NOT be able to change it:
`SET` and `RESET` of it fail with SQLSTATE 55P02, and so does making
a function with a `SET` clause for it; `RESET ALL` leaves it; a value
given at connection start fails with SQLSTATE 22023.

#### Scenario: Inside a parallel plan

- **WHEN** a counting `EXPLAIN` of a parallel query calls, above its
  Gather, a function that runs
  `EXPLAIN (ANALYZE, PMU, VERBOSE)` of a parallel `count(*)`
- **THEN** both succeed; the inner output shows launched workers and a
  positive part of the workers, not larger than its counts of the
  execution; the outer counts of the execution are larger than the
  inner ones
- **Verified by:** pending

#### Scenario: A nested statement fails

- **WHEN** a counting `EXPLAIN (ANALYZE, PMU, VERBOSE)` of a serial
  query calls a function that runs a nested counting EXPLAIN of a
  parallel query, which fails, and catches the error
- **THEN** the outer statement succeeds and shows a positive part of
  the workers
- **Verified by:** pending

#### Scenario: Two nested statements fail together

- **WHEN** a counting `EXPLAIN` of a serial query calls a function
  that catches errors around a nested counting EXPLAIN, whose function
  runs a second nested counting EXPLAIN of a parallel query, which
  fails
- **THEN** the outer statement succeeds and shows a positive part of
  the workers
- **Verified by:** pending

#### Scenario: No free place

- **WHEN** the server runs with `pg_pmu.max_nested` set to 2, and
  three nested counting statements would be open at once
- **THEN** the third fails with SQLSTATE 53000 before it runs, and a
  nested statement after it works again
- **Verified by:** pending

#### Scenario: The setting of the module

- **WHEN** a session runs `SET pg_pmu.cell = 1`, `RESET pg_pmu.cell`,
  and makes a function with `SET pg_pmu.cell = 1`
- **THEN** each fails with SQLSTATE 55P02, and `RESET ALL` succeeds
  and leaves the setting as it was
- **Verified by:** pending

### Requirement: Counting on each system

On Linux `pg_pmu` SHALL count with the kernel's performance events of
the thread that runs each process, in user space only. The work of the
kernel for the process and of threads a library starts is not counted.
A successful opening and reading of the events decides whether the
module counts. The usual conditions are `kernel.perf_event_paranoid`
of 2 or less, or the capability `CAP_PERFMON` or `CAP_SYS_ADMIN` of
the server's processes. Some distributions add values above 2 that
refuse processes without privileges, and the filters of system calls
in containers and the security modules can refuse too. On a processor
with several kinds of cores it SHALL count on all of them; it finds
the kinds in sysfs and SHALL fail with SQLSTATE 0A000 when they do not
cover all the processors online when it opens the counters, or when
the kernel cannot open
an event on one kind of cores: on Intel that needs Linux 5.13 or
later, on Arm a later kernel.

On macOS `pg_pmu` SHALL count with the counters the kernel keeps for
each process: all its threads, user space and kernel together. It
SHALL need no privileges. When the kernel reports no instructions for
the process, as in a virtual machine, it SHALL fail with SQLSTATE
0A000. Counts from Linux and from macOS mean different things and are
not comparable.

#### Scenario: Linux, counting allowed

- **WHEN** the Linux kernel lets the process open and read the events
- **THEN** `EXPLAIN (ANALYZE, PMU)` shows the counts
- **Verified by:** pending

#### Scenario: Linux, counting refused

- **WHEN** the kernel refuses the events with EACCES or EPERM, as an
  Ubuntu kernel does with `kernel.perf_event_paranoid` set to 4
- **THEN** the statement fails with SQLSTATE 42501, the error of the
  system, and a hint that names the setting, the capabilities and the
  filters of containers
- **Verified by:** pending

#### Scenario: Apple silicon

- **WHEN** the server runs on a Mac with Apple silicon, not in a
  virtual machine
- **THEN** `EXPLAIN (ANALYZE, PMU)` shows the counts
- **Verified by:** pending

#### Scenario: A virtual machine with macOS

- **WHEN** the server runs in a virtual machine with macOS whose
  kernel reports no instructions
- **THEN** the statement fails with SQLSTATE 0A000
- **Verified by:** pending

### Requirement: When the system cannot count

The backend SHALL open its counters when EXPLAIN has read its options.
When it cannot count, the statement SHALL fail before it runs, with
one of these SQLSTATEs:

- 42501: the system does not allow the process to count;
- 0A000: the process cannot count here: a virtual machine without
  counters, a system other than Linux and macOS, or kinds of cores
  that cannot all be counted;
- 53000: no hardware counter is free, no file descriptor is left, or
  no place is free for a nested statement.

Two failures come later, after a part of the statement has run. A
parallel worker that cannot count SHALL fail with the same error, and
so does the statement. On Linux, a counter that the system takes away
while the statement runs SHALL fail the statement with SQLSTATE 53000
at its end. A function may catch the error of a worker that could not
count; a counting statement around it SHALL still fail at its end,
with the same SQLSTATE, rather than show counts without that worker.
An ordinary error of a worker is not such a failure: what the worker
did until then is counted. In all these cases the next statement
counts again.

#### Scenario: Nothing runs

- **WHEN** a session on a system that cannot count runs
  `EXPLAIN (ANALYZE, PMU) SELECT nextval('s')`
- **THEN** the statement fails with one of the SQLSTATEs above, and
  the sequence `s` is not advanced
- **Verified by:** pending

#### Scenario: A caught failure of a worker

- **WHEN** a worker cannot count, and a function inside a counting
  statement catches its error
- **THEN** the counting statement fails at its end with the worker's
  SQLSTATE
- **Verified by:** pending

### Requirement: The shared memory of the workers

The module SHALL request its shared memory when the server starts,
under the name "pg_pmu", and its size SHALL NOT change while the
server runs. It holds one cell for each backend the server can run at
once, by the number PostgreSQL gives the backend, and a pool of
`pg_pmu.max_nested` cells for nested statements:

```
"pg_pmu": 16 + 32 * (N + K) bytes
N: the backends the server can run at once
K: pg_pmu.max_nested

offset        size    field
0             4       N
4             4       K
8             8       zero
16            32 * N  cells of the backends, cell i of backend i
16 + 32 * N   32 * K  cells of the pool

Cell: 32 bytes

offset  size  field
     0     8  instructions of the workers, summed
     8     8  cycles of the workers, summed
    16     8  state: the generation G in bits 20 to 63,
              the writers W in bits 0 to 19
    24     4  workers that could not count, counted
    28     4  SQLSTATE of the last of them

Token: a 64-bit number, written in decimal as the value of
pg_pmu.cell; 0 for no cell

bit     63 ... 20    19 ... 0
        G            number of the cell + 1
```

N is the number PostgreSQL computes for the backends; in PostgreSQL 19
and master it is the sum of `max_connections`,
`autovacuum_worker_slots`, `max_worker_processes`, `max_wal_senders`
and 2. PostgreSQL allows fewer than 2^18 backends and fewer than 2^18
workers, so N + K + 1 and the largest W fit in 20 bits, and G has 44.
All the fields of a cell are atomic integers; the module SHALL NOT
build where the processor has no native 64-bit atomics.

The state of a cell SHALL be one of these three:

- G even and W 0: the cell is free. The cell of a backend is taken by
  its outermost counting statement, a cell of the pool by a nested
  one.
- G odd and W below 2^20 - 1: the cell is taken, and W workers are
  adding to it at this moment.
- G even and W 2^20 - 1: the cell is being emptied into the statement
  around it; nobody may add to it or take it.

G counts modulo 2^44. A token is valid when its number of the cell is
from 1 to N + K and its G is odd; its decimal form has digits only,
with no sign or spaces, and is below 2^64. A report is accepted only
with a valid token whose G, all 44 bits of it, equals the G of the
cell, and only while W stays below 2^20 - 1. Any other report SHALL be
refused, add nothing, and fail the worker with SQLSTATE XX000, rather
than add to a cell that another statement may hold.

A cell that a worker is still adding to SHALL NOT be returned: that is
an internal error of the module, which fails the statement with
SQLSTATE XX000, or, when the statement is already failing, leaves the
cell taken until the server restarts and writes a warning to the log.
When that cell is the cell of a backend, every later outermost
counting statement of a backend with that number SHALL fail with
SQLSTATE XX000 before it runs, until the restart.

#### Scenario: The size at the start

- **WHEN** the server starts with `pg_pmu.max_nested` set to K
- **THEN** the column `size` of `pg_shmem_allocations` shows "pg_pmu"
  with 16 + 32 * (N + K) bytes
- **Verified by:** pending

#### Scenario: A report after the return

- **WHEN** a worker reports with the token of a cell that has been
  returned, and taken again or not
- **THEN** the report is refused and the sums of the cell do not
  change
- **Verified by:** pending

#### Scenario: A token that is not valid

- **WHEN** a report comes with a token whose number of the cell is 0
  or above N + K, whose G is even, or whose decimal form is not a
  number below 2^64
- **THEN** the report is refused and no cell changes
- **Verified by:** pending

#### Scenario: A return while a worker adds

- **WHEN** a statement returns its cell while a worker is between
  raising and lowering W
- **THEN** the return is refused, and the worker's addition is kept
- **Verified by:** pending
