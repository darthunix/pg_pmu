-- The option PMU, on a server that preloads pg_pmu. Nothing here
-- needs counters.

-- On a server that preloads the module, LOAD finds it loaded and does
-- nothing.
LOAD 'pg_pmu';

\set VERBOSITY sqlstate
EXPLAIN (PMU) SELECT 1;
EXPLAIN (PMU true, ANALYZE false) SELECT 1;
\set VERBOSITY default
EXPLAIN (PMU) SELECT 1;

-- The option takes the values of the boolean options of PostgreSQL.
\set VERBOSITY sqlstate
EXPLAIN (PMU maybe) SELECT 1;
\set VERBOSITY default

EXPLAIN (ANALYZE, PMU off, COSTS off, TIMING off, SUMMARY off,
         BUFFERS off) SELECT 1;
EXPLAIN (ANALYZE, PMU 0, COSTS off, TIMING off, SUMMARY off,
         BUFFERS off) SELECT 1;
EXPLAIN (ANALYZE, COSTS off, TIMING off, SUMMARY off, BUFFERS off)
  SELECT 1;
EXPLAIN (PMU false) SELECT 1;

LOAD 'auto_explain';
\set VERBOSITY sqlstate
SET auto_explain.log_extension_options = 'pmu';
SET auto_explain.log_extension_options = 'pmu on';
\set VERBOSITY default
SET auto_explain.log_extension_options = 'pmu';

SELECT size = 16 + 64 * (current_setting('max_connections')::int
                         + current_setting('autovacuum_worker_slots')::int
                         + current_setting('max_worker_processes')::int
                         + current_setting('max_wal_senders')::int + 2
                         + current_setting('pg_pmu.max_nested')::int)
       AS size_as_documented
FROM pg_shmem_allocations WHERE name = 'pg_pmu';

\set VERBOSITY sqlstate
SET pg_pmu.cell = 1;
BEGIN;
SET LOCAL pg_pmu.cell = 1;
ROLLBACK;
RESET pg_pmu.cell;
SELECT set_config('pg_pmu.cell', '1', false);
SELECT set_config('pg_pmu.cell', NULL, false);
CREATE FUNCTION set_cell() RETURNS int LANGUAGE sql
  SET pg_pmu.cell = 1 AS 'SELECT 1';
ALTER ROLE CURRENT_USER SET pg_pmu.cell = 1;
ALTER SYSTEM SET pg_pmu.cell = 1;
\set VERBOSITY default
RESET ALL;
SHOW pg_pmu.cell;
SELECT count(*) AS listed FROM pg_settings WHERE name = 'pg_pmu.cell';
