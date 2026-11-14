# Design

## Context

`pg_pmu` adds the option `PMU` to EXPLAIN ANALYZE. With it, EXPLAIN
shows how many instructions and cycles of the processor one statement
took, in the backend and in its parallel workers. Its first use is to
compare two executors of the same query by their work. What the module
promises is in specs/explain-pmu/spec.md. The module is written in C,
with the cells of its shared memory and its counters of the system in
a Rust crate.

The module needs these things from PostgreSQL. All of them are in
PostgreSQL 19 and in master, with the same signatures:

- an extension can add an option to EXPLAIN, keep its own state in the
  state of each EXPLAIN, and check all the options when they have been
  read;
- the hook of the planner receives the state of the EXPLAIN that asked
  for the plan, or none;
- a hook runs for each plan after EXPLAIN ANALYZE has run it and
  printed it, and before the executor ends;
- the hook of utility statements runs around each EXPLAIN;
- the workers receive a copy of the settings of their backend, made
  once when PostgreSQL makes their parallel context;
- a module in `shared_preload_libraries` can request shared memory
  of a fixed size when the server starts.

## Goals / Non-Goals

**Goals:**

- Counts of one statement, the parallel workers included, in the
  output of EXPLAIN ANALYZE.
- A fixed small cost: a few reads of the counters for each process and
  statement, whatever the number of rows; nothing at all for a
  statement without the option.
- Nothing to install in the database, and shared memory of a fixed
  size known before the start of the server.
- Counts that are right or an error, never a plausible wrong number.

**Non-Goals:**

- Counts for each node of the plan. Two reads of the counters for each
  row of each node would cost more than the work of many nodes, and
  the counts would mostly measure the counting.
- A table of statistics over many statements in shared memory.
- Systems other than Linux and macOS.

## Decisions

### The work of one EXPLAIN

The order of the work of EXPLAIN ANALYZE for one plan, and where the
module counts:

```
backend           pg_pmu            parallel worker
----------------  ----------------  ------------------------------
EXPLAIN begins    a frame begins
read the options  open counters,
                  take a cell
plan              count planning
start executor ┐  read counters
run            │  and the cell      start executor ┐
  Gather ──────┼──────────────────> run            │ count, add to
  waits for    │                    finish         ┘ its cell
  the workers  │                    end executor
finish         ┘  read both again
print the plan
print counts
end executor
print "Execution Time"
EXPLAIN ends      the frame ends,
                  the cell is
                  given back
```

The sections below take the pieces one by one: the module and its
loading, the frame of an EXPLAIN, how the statement is found among the
queries it runs, the windows of counting, the workers and their cells,
the counters of each system, and the shared memory.

### A module of the start of the server with an option of EXPLAIN

The counts belong to one statement that the user runs on purpose, and
EXPLAIN ANALYZE is where PostgreSQL shows what one run cost. An option
keeps the cost at zero when it is not given.

The module is loaded with `shared_preload_libraries`, by the
maintainer's decision. So it requests its shared memory when the
server starts, with a size it knows then, and `pg_pmu.max_nested` can
be a setting of the start: PostgreSQL lets a module define such a
setting only while it loads the modules of the start. Every process of
the server is forked from the postmaster with the module already
loaded, so all of them run the same version of it, and the memory
needs no mark of its version.

The first thing the module's initialization does is to check that the
server is loading the modules of the start, and to fail with 55000
otherwise. It must come first: defining a setting of the start later
is a fatal error of PostgreSQL, and requesting shared memory later is
an internal one, neither with a useful message. A failed initialization
leaves the module off the list of loaded libraries, so a second `LOAD`
fails the same way. With `session_preload_libraries` the failure comes
at the start of each connection that has the setting, which then
cannot connect. A `LOAD` on a server that preloads the module finds it
loaded and does nothing.

Refused: `LOAD` without a restart, with a named segment of dynamic
shared memory made on first use. The size of the pool could not then
be a setting fixed at the start, and a module replaced on disk could
meet memory of another version.

Refused: new columns in pg_stat_kcache. It sums each normalized query
over all its runs in a table in shared memory, so one run cannot be
read apart from the others without a reset. Refused: `perf stat -p`
outside the server. It does not know where a statement begins and
ends, and the parallel workers are other processes.

### A frame for each EXPLAIN

The hook of utility statements puts a frame on a stack when an EXPLAIN
begins, and takes it off when the EXPLAIN returns or fails. The handler
of the option marks the top frame as counting and keeps a pointer to
it in the state of the EXPLAIN. The frame holds the candidates below,
its reference to the counters, its cell and the sum of its nested
statements. An EXPLAIN that a function runs while another EXPLAIN runs
gets a frame above it.

The cell is passed in a setting, so the frame opens a nesting level of
the settings, saves the cell there, and closes the nesting level when
the EXPLAIN returns. When the EXPLAIN fails, the abort of the
transaction or the subtransaction closes it; the frame does not close
it again.

A failed frame leaves the stack before PostgreSQL has waited for its
workers, so what must outlive it does not live in it. A counting frame
keeps a hold of its cell apart from the frame: the token, the
subtransaction in which it took the cell, the sum of its own nested
statements, and the hold of the counting statement below it, if any.
Holds point to holds, never to frames, so a chain of holds survives
when several frames fail at once. The frame reads and adds to its sum
of nested statements through its hold. A hold lives until its cell
goes back, as the section on the workers says.

Refused: a callback on the memory of the EXPLAIN. Its state lives in
whatever memory is current, and one call of SPI can run an EXPLAIN and
then other statements before that memory is freed, so the state would
outlive its statement.

### Finding the statement among the queries it runs

Functions run their own queries, and they start the executor too, so
the module must tell the statement of EXPLAIN from them. It keeps a
depth: one more inside the planner and inside each start, run and
finish of the executor, restored when an error leaves them. A frame
remembers the depth of its EXPLAIN. Each start of the executor at that
depth, while the frame counts, is a candidate: the module reads the
counters and records the query and its plan. The finish of the same
query reads the counters again and completes the record. At both
points the record also takes the sums and the count of failures of the
frame's cell and the frame's sum of nested statements, so what a
function runs while the plan is printed cannot change them. The end
of the executor of a query, in its hook, removes the query's record.
The printing hook runs after the statement's finish and before its
end, so it takes the one completed record of the plan it prints whose
query has not ended, and then forgets all the records of the frame.

The queries at the depth of the EXPLAIN are of three kinds, and none
of them can be taken for the statement:

- those that compute the parameters of `EXPLAIN EXECUTE`: they end
  before the statement starts, so their records are gone;
- those that a function runs while the plan is printed, as an output
  function of a type or a custom scan can: they start after the
  statement has finished and end before the printing hook runs, so
  their records are gone too, even when they run the same plan, as a
  custom scan that executes the same prepared statement does;
- a cursor that a parameter opens: it does not finish inside the
  frame, so its record is never completed.

The queries run inside the statement, while it is planned or run, are
deeper.

The printing hook also runs for EXPLAIN without the option and for the
plans auto_explain logs. It does nothing when the state of the EXPLAIN
has the option off. With the option on and no completed record of the
plan, it raises an internal error rather than show numbers of another
query.

Refused: the last candidate alone, or the last completed record of the
plan. A query run while the plan is printed would take its place, also
with the same plan. Refused: matching the text of the query.
`EXPLAIN EXECUTE` passes the text of `PREPARE`, not its own. Refused:
matching on the options of instrumentation of the executor.
auto_explain sets them for nested queries too.

### The window of the execution

The backend reads its counters when the executor starts the statement
and when it has finished it, AFTER triggers included. The printing
hook runs before the executor ends, so the end of the executor cannot
be counted in the same output; it is left out. The ends of the queries
the statement runs inside fall in the window and are counted.

A worker uses the same window around its execution of the plan: the
one whose rows go to the tuple queue of the backend's Gather. It is
not picked by its depth: the worker may run queries while it starts,
before that execution, and a worker of a parallel index build runs
the queries of its functions with no plan of a query around them.
Neither is counted.

### Counts of the planning

The hook of the planner sees the state of the EXPLAIN. When it has the
option on, the hook reads the counters around the planner and keeps
the counts and the plan it made in that state. The printing hook shows
them for that plan. The cache of prepared statements calls the planner
with no state of EXPLAIN, so the planning of `EXPLAIN EXECUTE` is not
counted, even when the cache makes the plan during this EXPLAIN. The
planning is counted in the backend only: the planner starts no
workers.

### The parallel workers

The backend and its workers are separate processes, and each counts
only itself. Each counting statement has a cell in the shared memory
"pg_pmu" (the layout is in the spec), and its workers add to that
cell. The setting `pg_pmu.cell` tells a worker which cell it is.

PostgreSQL lets a backend that already runs a parallel plan start
another one from a function above the Gather, and launch its workers:
the planner refuses parallel plans only inside a worker, and EXPLAIN
plans its query with parallel plans allowed. Its documentation says
otherwise, but the code is what counts. So while a nested statement
runs, the workers of the outer statement may still run and add, and
one cell for both could not tell them apart. That is why a nested
statement has a cell of its own.

PostgreSQL copies the settings of the backend into a parallel context
once, when it makes the context at the first run of its Gather, and
not again when the Gather is run again. So each worker carries the
cell that was current when its context was made, however late it
starts.

- The outermost counting statement of a backend takes the cell of the
  backend, by its number. A nested counting statement takes a free
  cell of the pool; when none is free, it fails with 53000 before it
  runs. Taking is the same for both: a compare and swap of the state
  from an even generation with no writers to the next, odd, one. Only
  the backend itself takes its own cell, so there the swap loses only
  when a fault of the module left the cell taken, and the statement
  then fails with an internal error before it runs; in the pool two
  backends may race for a cell, and the loser tries the next one.
- The frame saves the token of its cell in `pg_pmu.cell`: the number
  of the cell and its whole generation. In a parallel worker
  a frame takes no cell and changes nothing: a worker cannot launch
  workers, and its part of the workers is zero.
- A worker counts only when its `pg_pmu.cell` is above 0. It takes
  the token when it starts to count, and fails with an internal error
  if the number is beyond the memory. It reports once: at the end of
  its finish, or, when it fails before, from a callback at its exit.
  That callback runs before the other callbacks of the exit, while the
  process is whole; any callback would be in time, as the backend
  waits for a failed worker to exit before it goes on.
- A report enters the cell first: a compare and swap raises the
  writers while the generation still matches the token and the
  writers stay below the mark of emptying less one. Then the worker
  adds its instructions and cycles, or its failure, and leaves by
  lowering the writers. A report with a token that is not valid, or
  whose generation does not match, fails with an internal error and
  adds nothing. As the check and the entry are one atomic step, no
  report can slip in between a return and a new taking.
- The record of a candidate reads the frame's cell at the start and
  at the finish of the statement; the part of the workers is the
  difference. A read loads the state, requires the generation of the
  token and no writers, and only then loads the sums and the count of
  failures; writers at that moment are the same fault as at a close.
  The difference, not the value, because the queries of one frame that
  run outside the statement, such as those that compute the
  parameters of `EXPLAIN EXECUTE`, report to the same cell.
- Every atomic operation on a cell is sequentially consistent. A
  statement does a handful of them, so weaker orders would save
  nothing, and with them the additions of a worker are visible to
  whoever sees it leave, and the zeroing of a cell to whoever takes it
  next.

The cell of a statement holds exactly the workers of the statement
and of the queries inside it, for two reasons:

- A context made while a statement is the innermost counting one
  carries its cell, and no other context does. A context made before
  the statement started belongs to an outer run, and keeps the cell
  of that run.
- Every context made in a statement's window is gone before the
  statement's finish. A complete run shuts its Gathers down, which
  waits for the workers, before it returns. When a function catches an
  error, the abort of its subtransaction destroys the contexts made
  inside it and waits for the workers to exit, before the handler
  runs.

A cell goes back when nobody can still write to it, and the statement
around it then receives all of it:

- At the normal end of a statement all its workers have exited, and
  the cell goes back at once.
- When a statement fails, the error reaches the module before
  PostgreSQL has waited for the workers, and a failed worker may still
  add from its exit callback. PostgreSQL waits, in the abort of a
  subtransaction, only for the contexts made in that subtransaction.
  So the cell goes back from the callback of the abort of the
  subtransaction in which the frame took it, or of the transaction;
  the callbacks of deeper subtransactions leave it. When several holds
  of one subtransaction go back in one callback, they go innermost
  first, so each hands down into a hold that is still there.
- Going back is three steps. A compare and swap closes the cell: from
  the odd generation with no writers to the next, even, one, with the
  writers set to the mark of emptying. From this moment every old
  token is refused, and nobody can take the cell. Then the cell's sums
  and count of failures, and the hold's sum of nested statements, are
  added to the sum of nested statements of the hold below, and the
  sums and the count of failures of the cell are set to zero; the
  state is left as it is. Last, the writers go from the mark to 0, and
  the cell is free.
- The whole cell is handed down, not the part of one statement: the
  nested frame's other queries, such as its parameters, ran inside the
  outer statement. The outermost statement has no hold below and
  hands it to nobody.
- If the writers are not 0 when the cell is closed, a worker is still
  adding, which the waits of PostgreSQL rule out; it is a fault of the
  module. At the normal end the statement fails with an internal
  error; in the callback of an abort, where no error may be raised,
  the module leaves the cell taken and writes a warning to the log.
  A cell of a backend left so fails every later outermost statement of
  a backend with that number, until a restart.

A backend that exits in a usual way, a fatal error or a termination
included, aborts its transaction first, which waits for its workers
and returns its cells. A crash of a process makes PostgreSQL start the
shared memory anew.

The generation is not needed for counts to be right: the waits of
PostgreSQL already keep every worker out of a cell after its return.
It is there to catch a fault of the module's own bookkeeping, such as
a cell returned too early, which would otherwise add one session's
work silently into another's. The generation has 44 bits, grows by
2 for each use of the cell and wraps around modulo 2^44, so a stale
report passes only if exactly
2^43 uses of the same cell fall between its token and its report:
fourteen years at twenty thousand statements a second. A token with
12 bits of it, which an integer setting of 32 bits would allow beside
the number of the cell, would pass after 2048 uses, a tenth of a
second of short statements in a row; that is why the setting holds a
string with the whole 64-bit token. The 20 bits of the writers hold
every worker PostgreSQL can start, as it allows fewer than 2^18.

The workers' failures to count are kept in the cell too. A worker
that cannot open or read its counters raises the count of failures in
its cell and stores its SQLSTATE, and then raises its error. A record
takes that count at the start and the finish, and the statement fails
at its finish with the stored SQLSTATE when it grew. So a function
that catches the error cannot make the statement around it show
counts without that worker.

The setting is for the module alone. It is a setting of the start of
a connection, so PostgreSQL itself refuses `SET`, `RESET` and the
`SET` clause of a function for it with 55P02, and `RESET ALL` does
not touch it. The module changes it from inside, with the context of
the start of a connection, which PostgreSQL allows, and the copy into
a worker passes the same way. Its check refuses a value given at the
start of a connection, with 22023, and lets through only the module's
own changes and the copy into a worker while it starts. It is hidden
from `SHOW ALL`, from the sample configuration and from the
configuration file. At the finish of each statement the frame checks
that the setting still holds its token, as a check of the module
itself, and fails with an internal error if it does not.

Refused: one cell for each backend with three cells for levels of
nesting inside parallel plans. It allows only three such levels,
although PostgreSQL has no limit but the depth of the stack, and its
proof needs more facts of PostgreSQL than one cell for each statement.
Refused: cells allocated from a dynamic shared area on demand. The
area starts at 1 MB, and the module would allocate and free memory for
each statement; a pool of fixed size set before the start is enough.
Refused: a nested statement that shares the cell of the outer one, or
ignores its own workers, or runs without workers. Its counts would
take the outer workers in, lose its own, or come from another run.
Refused: a setting that a check alone protects. A `RESET` takes the
default value without calling the check.

### Counting on Linux

The module counts with one group of two performance events,
instructions and cycles, for the thread of the process, on any CPU.

- The group counts user space only, with neither the kernel nor the
  hypervisor, which the mainline kernel allows at
  `perf_event_paranoid` 2, its default.
- It is pinned: the kernel keeps it on hardware counters all the time,
  or puts it into an error state where a read returns no bytes. Counts
  are never scaled from a part of the time.
- A read returns the number of events and their values in one call. A
  read that returns less, or a wrong number of events, fails the
  statement with 53000. A value is never reused after a failed read.
- The group is opened disabled once in each process, at the first
  statement that counts, and checked there: enabled, read and
  disabled. Each frame that counts and each counting execution of a
  worker takes a reference to it; the group is enabled when the first
  reference is taken and disabled when the last is given back, so an
  inner frame does not turn off the counters an outer one still reads.
  A disabled group costs nothing when the kernel switches processes,
  and enabling it again takes it out of an error state, so the next
  statement counts again.
- Its descriptors come from the budget of PostgreSQL for external
  files, so the backend does not run out of descriptors for its own
  files: the crate says how many it needs, and C takes them before the
  crate opens the groups. The crate owns the descriptors, C owns the
  budget. When an opening fails halfway, the crate closes what it has
  opened and C gives the whole budget back, before the error; the next
  statement tries again from the start. Once open, the groups live as
  long as the process, and the budget with them.

All of this is in the Rust crate, and so is the rule below; C only
asks it to open, take, give back and read.

`libc` has the system call but not the structure `perf_event_attr` or
the constants of perf events, so the crate declares the few it uses.
A wrong offset or bit there would count something else without a
sign, so a C file of the module includes `<linux/perf_event.h>` beside
`pg_pmu_core.h` and, when the module loads, compares with it what the
crate reports it uses: the size of the structure, its offsets, the
word of bit fields the crate sets, and the values of the constants and
of the requests of `ioctl`. A mismatch stops the server's start. The
check runs at load rather than at compile time because bit fields
have no offsets in C; a structure is filled with them and its word
compared.

On a processor with several kinds of cores each kind has its own unit
in `/sys/bus/event_source/devices`, with a file `cpus` that lists its
processors. The rule:

- more than one such unit: one group for each, with the type of the
  unit in bits 32 to 63 of the event's configuration, as the kernel
  takes it for hardware events since Linux 5.13 on Intel; the counts
  of the groups are added; more than four units fail with 0A000;
- one unit, or none, as on x86 with one kind of cores, where the unit
  has no file `cpus`: one group without a type;
- the processors of the units must cover the processors online when
  the groups are opened, from `devices/system/cpu/online`, and with no
  unit or no sysfs the processor must say it has one kind of cores, as
  x86 does; otherwise the module fails with 0A000, as one group would
  count on one kind of cores only and nothing would tell. The possible
  processors are not used: machines in clouds list slots that are not
  online.

This follows the instruction counter of Tarantool, with the check of
the coverage added. The crate is given the root of sysfs, so the tests
can hand it one they made. A processor of a new kind brought online
after the groups are open is not counted; one of a kind already open
is, as an event of a unit counts on all its processors.

A per-thread event counts only the thread that opened it. A backend
runs in one thread; a thread that a library starts is not counted,
unlike on macOS.

Refused: reading the counters with the instruction `rdpmc` from a page
the kernel maps. It is faster than a system call, but a statement needs
a few reads. Refused: counting the kernel too, which needs
`perf_event_paranoid` 1 or less. Refused: events inherited by new
threads, which the kernel does not allow with reads of a group.

### Counting on macOS

The crate calls `proc_pid_rusage` for its own process with version 4
of the structure and takes `ri_instructions` and `ri_cycles`. These
are the counters the kernel keeps for each thread, summed over the
process, user space and kernel together. The call is part of the
public `libproc.h` and needs no privileges. A virtual machine reports
zero; the module checks the instructions when it opens the counters,
as the process has already run millions of them, and fails with 0A000
if they are zero. Whether an Intel Mac reports them is to be checked
on one.

Refused: `thread_selfcounts`, which is not declared in the SDK.
Refused: kperf, which needs root, and PostgreSQL refuses to run as
root.

Counts from Linux and from macOS mean different things: the kernel and
other threads are in one and not in the other. They are not compared
with each other; the README says so.

### Errors and their timing

The backend opens and checks its counters when EXPLAIN has read its
options, so a system where the backend cannot count fails the
statement before it runs. A worker opens its counters when it starts,
after a part of the statement has run; when it cannot, it records the
failure in its cell and raises the same error, and PostgreSQL raises
it again in the backend. When a function catches that error, the
recorded failure still fails the statement around it at its finish.
An ordinary error of a worker is not a failure to count: its exit
callback adds what it counted, and the statement around a function
that catches the error goes on with complete counts. The codes are in
the spec.

### Settings of auto_explain

auto_explain can pass options of extensions to the plans it logs, and
PostgreSQL requires a check for each option an extension registers.
The module registers a check that refuses `PMU` with a message: the
plans auto_explain logs have already run, and nothing was counted.

### The shared memory

The module registers its memory with the callbacks of shared memory
that `pg_stat_statements` uses in PostgreSQL 19 and master, right
after the check of its initialization. PostgreSQL calls them after it
has computed the number of backends, so the size is known then: 16 +
32 * (N + K) bytes, N the number of backends the server can run at
once and K `pg_pmu.max_nested`. With the defaults of PostgreSQL N is
136, so the memory takes 4880 bytes with K = 16; each 16 cells more of
the pool cost 512 bytes. Neither number changes while the server runs,
so the memory is never resized or freed, and nothing is allocated for
a statement.

The pool is one for the whole server, not one for each backend: a
nested counting statement is rare, and a pool for each backend would
cost N times as much. A session can hold cells of the pool only while
its nested statements run.

### The Rust part

The cells of the shared memory and the counters of the system live in
a Rust crate, `pg_pmu_core`, built as a static library and linked
into the module. C keeps two things: getting memory and file
descriptors from PostgreSQL, and the work inside PostgreSQL, that is
the hooks, the settings, the frames and their records, and the
errors. The crate returns codes, with the error of the system where
there is one, and C turns them into errors of PostgreSQL with their
messages and hints.

```
             C: pg_pmu.c, workers.c
 ┌────────────────────────────────────────────────────┐
 │ hooks · settings · frames · holds · ereport        │
 │ shared memory → base address, N, K                 │
 │ budget of external files → how many descriptors    │
 └──────────────┬─────────────────────────────────────┘
                │ extern "C"
 ┌──────────────▼─────────────────────────────────────┐
 │ Rust: pg_pmu_core                                  │
 │ cells:    layout, token, take, enter, add, leave,  │
 │           close, empty, open, read                 │
 │ counters: Linux perf events, macOS proc_pid_rusage │
 │           kinds of cores, groups, references, reads│
 └────────────────────────────────────────────────────┘
```

The cells: the crate lays out the header and the cells, makes and
reads tokens, and does every step on a cell. C hands it the address,
N and K once, and never reads or writes a cell itself.

The counters: the crate finds the kinds of cores and checks their
coverage, opens the groups, keeps the references that enable and
disable them, reads and checks each read, and on macOS calls
`proc_pid_rusage`. Before it opens the groups it tells C how many
descriptors it needs, and C takes them from PostgreSQL's budget of
external files. It reads sysfs from a root it is given, so the tests
can hand it a directory they made.

The reason is checking. The steps on a cell are a protocol between
processes on atomics, and its faults show only in rare orders of
events, which an SQL suite almost never meets. The counters hold logic
that an SQL suite cannot reach either: lists of processors in sysfs,
machines with one, two or three kinds of cores, half-readable sysfs,
short reads. In Rust these tools reach them:

- loom runs models of the protocol, with threads in the place of
  processes, through every order of their atomic steps. loom allows
  five threads and its search grows fast with each, so there are
  several small models of two or three threads: reports against a
  close; a late report with an old token against a close and a new
  taking; two backends that race for one cell of the pool; a nested
  cell handed down into the hold below while a report comes. They
  check that no report of a statement is lost, no late report is
  taken, no cell is held twice, and what is handed down equals the sum
  of the reports. The steps are written once over an atomic type that
  `cfg(loom)` switches to loom's; the models run the same code as the
  module, and the tests of the layout run only without loom, as loom's
  atomics have other sizes. loom covers the orders of the modelled
  runs, not every program;
- proptest runs random sequences of the steps, of many backends and
  workers, against a plain model in ordinary memory, and compares the
  states and the sums after each step. For the counters it makes sysfs
  directories of random machines, with their lists of processors
  written in every form the kernel uses and some files missing or
  unreadable, and compares the groups the crate would open, or its
  refusal, with a model; and it runs random sequences of taking and
  giving back references against a model of when the group is on;
- Kani proves, for every input in a stated domain: the token, for
  every 64-bit value and N + K up to 2^20 - 1, is made and read back
  exactly, and only valid ones are accepted; its decimal form, for
  every string of up to 20 bytes, is read back to the same number or
  refused; each step, for every state word, leads from a state the
  spec allows to another one it allows, or refuses, with the
  generation wrapping modulo 2^44; a number of a list of processors,
  for every input of up to 12 bytes, is read without overflow and only
  when it is digits; the parser of a whole list, for every input of up
  to 8 bytes into a set of 128 processors, never panics, the code being
  the same for every size of set; adding a range, for every pair of
  numbers, adds exactly the processors between them; the check of a
  read, for every buffer of up to 64 bytes, accepts exactly a read of
  the right length with the right number of events. The loops are
  unwound to those bounds with the checks of unwinding on. The proof of
  the parser takes about twelve minutes; longer lists are left to
  proptest.
  Kani does not model system calls, sysfs or other threads; the steps
  are proved one at a time, as sequential code, and loom covers their
  interleavings;
- plain tests on a machine with counters check that the counts grow
  with the work done and that a disabled group does not count; where
  the system cannot count they check the codes instead.

The crate is `no_std` except under its tests, which need the
standard library, and is built both as a static library for the
module and as a library for its tests. It uses `core` and the crate
`libc`, without its default features, and allocates nothing. Its
`Cargo.lock` is kept in the repository; building needs crates.io or
sources vendored with `cargo vendor`, as the README says.

A panic in the crate would abort the process, and PostgreSQL treats
that as a crash and restarts every session. So no function of the
crate's interface may panic: the arithmetic of the generation wraps,
every other step checks its bounds and returns a code, slices are
not indexed without a check, and Clippy denies panics, `unwrap`,
indexing and unchecked arithmetic in the crate. Kani proves the
absence of panics for the functions it proves; for the rest, the
README says what a panic would mean.

The crate refuses to build where the target has no 64-bit atomics,
which takes the place of the check on PostgreSQL's atomics; the
module does not use those at all. Its types are `#[repr(C)]`, and its
tests check their sizes and offsets against the layout in the spec.
On the processors the module supports, x86-64 and AArch64, the atomic
instructions behave on memory shared between processes as between
threads, so a model with threads checks the same protocol. On Linux
and macOS the crate builds the counters of the system, on any other
target the version that cannot count, as it does with the feature
`none`. The C declarations of the crate's functions are in a header
written by hand, which a C file of the suite includes and calls.

Refused: the protocol in C on PostgreSQL's atomics. No tool runs
through its orders of events, and on a platform without 64-bit
atomics PostgreSQL hides a lock behind them. Refused: a crate for perf
events from crates.io. The module needs pinned groups in user space,
one for each kind of cores, with their error state checked; the
system calls for that are few, and `libc` has the call itself.

## What we do not do

- Counts without EXPLAIN. The counts include the work EXPLAIN ANALYZE
  does to measure each node, about a few dozen instructions for each
  row of each node. It weighs on an executor that passes rows one by
  one more than on one that passes them in batches. A way to count a
  statement run without EXPLAIN would remove it. The maintainer
  decided not to build it: the bias is accepted and documented.
- More nested counting statements at once than `pg_pmu.max_nested`.
  They fail with 53000, and the setting can be raised before the next
  start.
- A sweep of the pool for cells that nobody returned. The aborts that
  return them always run, as above, and a crash starts the memory
  anew.
- Counting the planning of prepared statements.
- Counts of each worker apart. The part of all the workers together is
  enough to see how the work was shared.
- The start of the worker processes. It is the same for two executors
  with the same number of workers.
- Other events of the processor, such as misses of the caches or
  branches. They can be added to the same group later.

## Risks / Trade-offs

- [The machines of CI may have no counters: virtual machines often
  give none to processes.] → The suite prints one line for any system
  that cannot count and has a second expected output for it; CI prints
  which one it matched. The counts are checked by hand on a machine
  with counters until CI has one.
- [Distributions, containers and security modules refuse the events
  in their own ways.] → The error carries the system's own error and
  a hint that names the setting, the capabilities and the filters of
  containers.
- [The hooks can be taken by another extension that does not call the
  previous one.] → The module calls the previous hooks, as every
  extension must; a plan without its record fails the statement with
  an internal error rather than show wrong numbers.
- [The counts include the instrumentation of EXPLAIN ANALYZE.] → The
  spec and the README say so, and recommend `TIMING off`.
- [On a processor with two kinds of cores, the group of one kind is
  pinned while the thread runs on the other kind. The design assumes
  the kernel skips it there, as perf's filter of units does, rather
  than put it into the error state, which would fail every statement
  with 53000.] → Checked first on an Intel machine with two kinds of
  cores.
- [A fault in the bookkeeping of held cells would leave cells of the
  pool taken until a restart.] → The tests return a cell after a
  failed nested statement and take it again.

## Decisions of the maintainer

- 2026-11-14: a separate repository, `pg_pmu`; counts of one statement
  in EXPLAIN ANALYZE rather than columns of pg_stat_kcache in a table
  in shared memory; counts of the whole statement, not of its nodes.
- 2026-11-14: no counting without EXPLAIN; the PostgreSQL License.
- 2026-11-14: parallel plans are counted with no refusal, statements
  started inside a parallel plan included.
- 2026-11-14: the module lives in `shared_preload_libraries`; the
  number of nested counting statements is a setting fixed before the
  start; memory of a fixed size, a cell for each backend and a pool
  for the nested statements.
- 2026-11-14: cells have generations, and a report with an old token
  is refused; the layout and the steps on it are in Rust, checked with
  loom, proptest and Kani; C keeps the allocation of the memory and
  the work inside PostgreSQL.
- 2026-11-14: the counters of the system are in the Rust crate too,
  with their own checks; the generation has 44 bits, and the token is
  a 64-bit number in a string setting. The maintainer kept the token
  and the writers over a simpler scheme without them, which would let
  a fault of the module through in two narrow windows.

## Files

- `Makefile`: the PGXS build of the module and its suite, with the
  crate built by Cargo and linked in; the variable
  `PG_PMU_COUNTERS=none` builds the crate with the feature `none`, the
  version that cannot count, and `PG_PMU_TEST=workers-cannot-count`
  the module whose parallel workers cannot count, for its suite.
- `crates/pg_pmu_core/`: the crate. `src/cells.rs`: the layout, the
  token and the steps. `src/counters/mod.rs`: the references;
  `perf.rs`: the groups of perf events over the calls of the system;
  `linux.rs`: those calls on Linux; `perf_abi.rs`: the few
  declarations of Linux's perf events; `macos.rs`, `none.rs`: the
  other systems. `src/sysfs.rs`: the kinds of cores and their
  coverage. `src/ffi.rs`: the functions C calls. The proofs of Kani
  are beside the code they prove. `tests/model.rs`: the tests of
  proptest against the plain models; `tests/loom.rs`: the models of
  loom; `tests/counters.rs`: the tests on real counters; `tests/abi.rs`:
  the codes of the header against the crate's; `Cargo.lock`.
- `pg_pmu_core.h`: the C declarations of the crate's functions.
- `pg_pmu.h`: what the two C files of the module share.
- `pg_pmu.c`: the option, the hooks, the frames, the depth, the
  records, the output, the errors of the counters.
- `workers.c`: the request of the shared memory, the setting
  `pg_pmu.cell`, the holds of cells, and the counting in the workers,
  all through the crate.
- `test/abi.c`: the C file of the module that includes
  `pg_pmu_core.h`, compares its types with the crate's, and on Linux
  compares `src/counters/perf_abi.rs` with `<linux/perf_event.h>`,
  when the module loads.
- `pg_pmu.conf`: the settings of the temporary server of the suite.
- `sql/`, `expected/`: the regression suite.
- `README.md`: loading, use, what the counts mean on each system.
- `.github/workflows/ci.yml`: builds and tests;
  `.github/postgres.env`: the revisions of PostgreSQL it builds;
  `.github/scripts/build-postgres.sh`: the build of PostgreSQL.

## Tests

The regression suite runs on a temporary server started with
`pg_pmu.conf`: the module in `shared_preload_libraries`,
`pg_pmu.max_nested` set to 2, so that two nested statements at once
work and a third finds no cell; the default number of workers is
enough for an outer and an inner parallel query at once. Counts change from run to run,
so the suite compares them, not their values: positive numbers, more
for a larger table, the part of the workers within the counts of the
execution. A function of the suite runs each EXPLAIN, replaces the
numbers of the text format with `N`, and prints one line, "cannot
count", when the statement fails with any of the three codes of a
system that cannot count. Each suite that needs counters has a second
expected output with that line; `sql/option.sql` needs none and has
one output.

The tests of the workers set `min_parallel_table_scan_size` and the
costs of parallel plans to zero, and check in the same output that
workers were launched. The suite also runs: an EXPLAIN followed by
other statements in one call of SPI; a parameter of `EXPLAIN EXECUTE`
that opens a cursor; an EXPLAIN that fails inside an `EXCEPTION`
block, and one that counts after it; a parallel query that fails
inside a function that catches the error; workers that run a function
with a query; a counting EXPLAIN inside a function above the Gather of
a counting EXPLAIN, both parallel; a counting EXPLAIN in a
parallel-safe function that a worker runs; a plain parallel query
whose function runs a counting EXPLAIN; a nested parallel EXPLAIN that
fails under a serial outer one, whose part of the workers must be
positive; the same inside two nested `EXCEPTION` blocks, and its cell
taken again after them; two nested counting statements that fail
together under a catch around both, whose outer part of the workers
must be positive; a third nested statement at once (53000), and a
nested one that works again after it; the size of "pg_pmu" in the
column `size` of `pg_shmem_allocations`; `SET`, `RESET`, `set_config`
with no value, and making a function with a `SET` clause for
`pg_pmu.cell` (55P02), and `RESET ALL`; one case with
`debug_parallel_query` set to `regress`, and the rest with it off.

The suite `sql/unloaded.sql` runs on a second temporary server without
the module: the option refused as unknown, and `LOAD` refused with
55000.

Three suites need a system of their own, and each has a target of its
own. `sql/paranoid.sql` runs on Linux with `kernel.perf_event_paranoid`
set to 4, where an Ubuntu kernel refuses the events: 42501 with its
hint. `sql/vm.sql` runs in a virtual machine with macOS: 0A000.
`sql/workers_cannot_count.sql` runs on a machine that counts, with a
build of the module whose parallel workers fail to open their
counters: a function that catches the error of such a worker does not
save the statement. Of the runners of CI those of AArch64 Linux count,
the x86-64 ones have no PMU, and those of macOS are virtual machines;
so CI demands counts on AArch64 Linux, runs the first and the third
suite there and the second on macOS.

The crate has its own checks, run in `crates/pg_pmu_core` by
`cargo test`, by `cargo test` with `--cfg loom` for the models of
loom, and by `cargo kani`: the sizes and offsets of the layout; a
report with an old token refused, also after the cell was taken again;
tokens that are not valid, and decimal forms that are not numbers or
are too large, refused; the generation at its last value wrapping; a
return refused while a worker is inside; two backends racing for one
cell; everything a statement's workers reported handed down whole;
an opening of counters that fails on its second group, closing the
first, and a next opening that succeeds. The refusal of the crate is
what its tests show; that C turns it into XX000 is shown by
`test/abi.c` or checked by review.

Checked by hand, as a test cannot make them: the error of Linux when
all counters are taken, and the recovery of the next statement after
it; counting on a Mac with Apple silicon, by the suites with
`installcheck-counted`; an Intel Mac; connections
refused with `session_preload_libraries`, and a connection that gives
`pg_pmu.cell` a value at its start (22023), whose failure pg_regress
cannot print in a stable form; SQL run while the plan is printed,
also with the same plan, which needs a type or a custom scan written
in C; a parallel index build whose expression runs a query inside a
counting statement.
