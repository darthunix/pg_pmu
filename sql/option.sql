-- The option PMU, on a server that preloads pg_pmu. Nothing here
-- needs counters.

-- spec: explain-pmu/Loaded at the start
-- On a server that preloads the module, LOAD finds it loaded and does
-- nothing.
LOAD 'pg_pmu';

-- spec: explain-pmu/Without ANALYZE
\set VERBOSITY sqlstate
EXPLAIN (PMU) SELECT 1;
EXPLAIN (PMU true, ANALYZE false) SELECT 1;
\set VERBOSITY default
EXPLAIN (PMU) SELECT 1;

-- The option takes the values of the boolean options of PostgreSQL.
\set VERBOSITY sqlstate
EXPLAIN (PMU maybe) SELECT 1;
\set VERBOSITY default

-- spec: explain-pmu/Turned off
EXPLAIN (ANALYZE, PMU off, COSTS off, TIMING off, SUMMARY off,
         BUFFERS off) SELECT 1;
EXPLAIN (ANALYZE, PMU 0, COSTS off, TIMING off, SUMMARY off,
         BUFFERS off) SELECT 1;
EXPLAIN (ANALYZE, COSTS off, TIMING off, SUMMARY off, BUFFERS off)
  SELECT 1;
EXPLAIN (PMU false) SELECT 1;

-- spec: explain-pmu/auto_explain
LOAD 'auto_explain';
\set VERBOSITY sqlstate
SET auto_explain.log_extension_options = 'pmu';
SET auto_explain.log_extension_options = 'pmu on';
\set VERBOSITY default
SET auto_explain.log_extension_options = 'pmu';

-- spec: explain-pmu/The size at the start
SELECT size = 16 + 32 * (current_setting('max_connections')::int
                         + current_setting('autovacuum_worker_slots')::int
                         + current_setting('max_worker_processes')::int
                         + current_setting('max_wal_senders')::int + 2
                         + current_setting('pg_pmu.max_nested')::int)
       AS size_as_the_spec_says
FROM pg_shmem_allocations WHERE name = 'pg_pmu';

-- spec: explain-pmu/The setting of the module
\set VERBOSITY sqlstate
SET pg_pmu.cell = 1;
RESET pg_pmu.cell;
SELECT set_config('pg_pmu.cell', '1', false);
SELECT set_config('pg_pmu.cell', NULL, false);
CREATE FUNCTION set_cell() RETURNS int LANGUAGE sql
  SET pg_pmu.cell = 1 AS 'SELECT 1';
\set VERBOSITY default
RESET ALL;
SHOW pg_pmu.cell;
SELECT count(*) AS listed FROM pg_settings WHERE name = 'pg_pmu.cell';
