-- PMU full: the branches and the cache at once, which needs six free
-- counters of a core. A system that counts them gives this output; any
-- other, as one with fewer counters free, macOS, a virtual machine or a
-- build without counters, gives events_full_1.out, where each counting
-- EXPLAIN prints "cannot count" and each fact is empty. The two
-- functions repeat those of the other suites of events, so that it
-- runs alone.

-- Run an EXPLAIN and return its lines, with the numbers of the lines of
-- pg_pmu replaced by N; or one line when the system cannot count.
CREATE OR REPLACE FUNCTION pmu_lines(query text) RETURNS SETOF text
LANGUAGE plpgsql AS $$
DECLARE
  line text;
BEGIN
  FOR line IN EXECUTE query LOOP
    RETURN NEXT CASE WHEN line ~ 'PMU'
                     THEN regexp_replace(line, '[0-9]+', 'N', 'g')
                     ELSE line END;
  END LOOP;
EXCEPTION
  WHEN OTHERS THEN
    IF SQLSTATE IN ('42501', '0A000', '53000') THEN
      RETURN NEXT 'cannot count';
    ELSE
      RAISE;
    END IF;
END
$$;

-- The document of an EXPLAIN in JSON, or NULL when the system cannot
-- count.
CREATE OR REPLACE FUNCTION pmu_doc(query text) RETURNS jsonb
LANGUAGE plpgsql AS $$
DECLARE
  doc text;
BEGIN
  EXECUTE query INTO doc;
  RETURN doc::jsonb -> 0;
EXCEPTION
  WHEN OTHERS THEN
    IF SQLSTATE IN ('42501', '0A000', '53000') THEN
      RETURN NULL;
    ELSE
      RAISE;
    END IF;
END
$$;

SET max_parallel_workers_per_gather = 0;
SET jit = off;
CREATE TABLE ev_full AS SELECT g FROM generate_series(1, 100000) g;
ANALYZE ev_full;

-- The value prints both pairs, in any case.
SELECT * FROM pmu_lines($$
  EXPLAIN (ANALYZE, PMU FULL, COSTS off, TIMING off, SUMMARY off,
           BUFFERS off)
  SELECT count(*) FROM ev_full$$);

SELECT k FROM jsonb_object_keys(pmu_doc($$
  EXPLAIN (ANALYZE, PMU full, FORMAT JSON) SELECT count(*) FROM ev_full$$)) AS k
WHERE k ~ '^(Planning|Execution) ' AND k !~ 'Time$'
ORDER BY k COLLATE "C";

-- The facts of both pairs in one statement.
SELECT (j ->> 'Execution Branches')::numeric >= 100000 AS a_branch_a_row,
       (j ->> 'Execution Branch Misses')::numeric
         < (j ->> 'Execution Branches')::numeric AS some_mispredicted,
       (j ->> 'Execution Cache References')::numeric > 0 AS cache_referred
FROM pmu_doc($$EXPLAIN (ANALYZE, PMU full, FORMAT JSON)
               SELECT count(*) FROM ev_full$$) AS j;

-- The workers count both.
RESET max_parallel_workers_per_gather;
SET parallel_setup_cost = 0;
SET parallel_tuple_cost = 0;
SET min_parallel_table_scan_size = 0;
SET parallel_leader_participation = off;
SELECT jsonb_path_query_first(j, '$.**."Workers Launched"')::int > 0
         AS launched,
       (j ->> 'Workers Branches')::numeric > 0 AS branches_counted,
       (j ->> 'Workers Cache References')::numeric > 0 AS cache_counted
FROM pmu_doc($$EXPLAIN (ANALYZE, PMU full, VERBOSE, FORMAT JSON)
               SELECT count(*) FROM ev_full$$) AS j;
RESET parallel_leader_participation;
