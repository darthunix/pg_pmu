-- Runs on a server that does not preload pg_pmu.

-- spec: explain-pmu/Not loaded
\set VERBOSITY sqlstate
EXPLAIN (ANALYZE, PMU) SELECT 1;

-- spec: explain-pmu/Loaded later
LOAD 'pg_pmu';
\set VERBOSITY default
-- A failed initialization is not kept, so a second LOAD fails the same.
LOAD 'pg_pmu';
