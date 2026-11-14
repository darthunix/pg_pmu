-- The option PMU, on a server that preloads pg_pmu. Nothing here
-- needs counters.

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
