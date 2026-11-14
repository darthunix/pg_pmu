-- With the build PG_PMU_TEST=workers-cannot-count, on a machine that
-- counts: the backend counts, and each parallel worker of a counting
-- statement fails to open its counters with 0A000.
SET parallel_setup_cost = 0;
SET parallel_tuple_cost = 0;
SET min_parallel_table_scan_size = 0;
SET parallel_leader_participation = off;
CREATE TABLE big AS SELECT g FROM generate_series(1, 100000) g;
ANALYZE big;

CREATE FUNCTION caught() RETURNS text
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM count(*) FROM big;
  RETURN 'no error';
EXCEPTION
  WHEN OTHERS THEN
    RETURN 'caught ' || SQLSTATE;
END
$$;

-- The workers of a statement that does not count do not count, and do
-- not fail.
SELECT caught();

-- The error of a worker fails the statement.
\set VERBOSITY sqlstate
EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, SUMMARY off)
  SELECT count(*) FROM big;

\set VERBOSITY default

-- case: A caught failure of a worker
DO $$
BEGIN
  EXECUTE 'EXPLAIN (ANALYZE, PMU) SELECT caught()';
EXCEPTION
  WHEN OTHERS THEN
    RAISE NOTICE '%: %', SQLSTATE, SQLERRM;
END
$$;

-- The next statement counts again.
DO $$
DECLARE
  line text;
  counted bool := false;
BEGIN
  FOR line IN EXECUTE 'EXPLAIN (ANALYZE, PMU) SELECT 1' LOOP
    counted := counted OR line ~ '^Execution PMU: instructions=[1-9]';
  END LOOP;
  RAISE NOTICE 'the next statement counted: %', counted;
END
$$;
