-- EXPLAIN (ANALYZE, PMU) on Greengage: the coordinator counts its part,
-- each QE its part of its slice, and the coordinator adds them in. The
-- plans are those of the Postgres-based planner.
SET optimizer = off;

-- Run an EXPLAIN and return its lines, with the counts of pg_pmu, the
-- segment of the most instructions and the times replaced by N, as they
-- change from run to run; the numbers of slices and of QEs stay.
CREATE FUNCTION pmu_explain(query text) RETURNS SETOF text
LANGUAGE plpgsql AS $$
DECLARE
  line text;
BEGIN
  FOR line IN EXECUTE query LOOP
    -- A document in JSON or YAML comes as one row of many lines.
    RETURN NEXT (
      SELECT string_agg(
               CASE WHEN l ~ 'PMU:'
                    THEN regexp_replace(
                           regexp_replace(l, '(instructions|cycles)=[0-9]+',
                                          '\1=N', 'g'),
                           '\(seg-?[0-9]+\)', '(segN)')
                    WHEN l ~ '(Instructions|Cycles|Max Segment|Time)'
                    THEN regexp_replace(l, '-?[0-9]+([.][0-9]+)?', 'N', 'g')
                    ELSE l END,
               E'\n' ORDER BY n)
      FROM regexp_split_to_table(line, E'\n') WITH ORDINALITY AS x(l, n));
  END LOOP;
END
$$;

-- The query of an EXPLAIN in JSON, to compare counts. The JSON is taken
-- with SUMMARY off: the core prints the statistics of the slice of an
-- initplan as "}_", which is not JSON.
CREATE FUNCTION pmu_json(query text) RETURNS jsonb
LANGUAGE plpgsql AS $$
DECLARE
  doc text;
BEGIN
  EXECUTE query INTO doc;
  RETURN doc::jsonb -> 0;
END
$$;

CREATE TABLE small (a int, b int) DISTRIBUTED BY (a);
CREATE TABLE big (a int, b int) DISTRIBUTED BY (a);
CREATE TABLE copy (a int, b int) DISTRIBUTED BY (b);
INSERT INTO small SELECT i, i % 10 FROM generate_series(1, 1000) i;
INSERT INTO big SELECT i, i % 10 FROM generate_series(1, 100000) i;
ANALYZE small, big;

-- The option needs ANALYZE.
EXPLAIN (PMU) SELECT count(*) FROM small;

-- DXL runs nothing, so it cannot count.
EXPLAIN (ANALYZE, PMU, DXL) SELECT count(*) FROM small;

-- Without the option nothing is printed.
EXPLAIN (ANALYZE, COSTS off, TIMING off, SUMMARY off)
SELECT count(*) FROM small;

-- The counts of the planning and of the execution: the coordinator's
-- and those of the QEs of slice 1.
SELECT * FROM pmu_explain($$
  EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, SUMMARY off)
  SELECT count(*) FROM small$$);

-- Under VERBOSE also the coordinator's part and each slice's: a root
-- slice that runs on the segments, slice 0 of an INSERT, a slice on the
-- segments under it, and an initplan dispatched on its own, slice 3.
SELECT * FROM pmu_explain($$
  EXPLAIN (ANALYZE, PMU, VERBOSE, COSTS off, TIMING off, SUMMARY off)
  INSERT INTO copy SELECT * FROM small$$);
SELECT * FROM pmu_explain($$
  EXPLAIN (ANALYZE, PMU, VERBOSE, COSTS off, TIMING off, SUMMARY off)
  SELECT count(*) FROM small WHERE a > (SELECT avg(a) FROM small)$$);

-- A plan that runs on the coordinator alone has no slice of the
-- segments.
SELECT * FROM pmu_explain($$
  EXPLAIN (ANALYZE, PMU, VERBOSE, COSTS off, TIMING off, SUMMARY off)
  SELECT 1$$);

-- A CREATE TABLE AS WITH NO DATA dispatches nothing, and needs no QE.
SELECT * FROM pmu_explain($$
  EXPLAIN (ANALYZE, PMU, VERBOSE, COSTS off, TIMING off, SUMMARY off)
  CREATE TABLE nodata AS SELECT * FROM small WITH NO DATA
  DISTRIBUTED BY (a)$$);

-- A query on one segment has one QE; a LIMIT that stops early still has
-- the reports of all.
SELECT * FROM pmu_explain($$
  EXPLAIN (ANALYZE, PMU, VERBOSE, COSTS off, TIMING off, SUMMARY off)
  SELECT * FROM small WHERE a = 5$$);
SELECT * FROM pmu_explain($$
  EXPLAIN (ANALYZE, PMU, VERBOSE, COSTS off, TIMING off, SUMMARY off)
  SELECT * FROM big LIMIT 1$$);

-- A plan from the cache of prepared statements has no planning counts.
PREPARE counted AS SELECT count(*) FROM small;
SELECT * FROM pmu_explain($$
  EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, SUMMARY off)
  EXECUTE counted$$);
DEALLOCATE counted;

-- The plans of GPORCA count the same way.
SET optimizer = on;
SELECT * FROM pmu_explain($$
  EXPLAIN (ANALYZE, PMU, VERBOSE, COSTS off, TIMING off, SUMMARY off)
  SELECT count(*) FROM small$$);
SET optimizer = off;

-- In the other formats, properties.
SELECT * FROM pmu_explain($$
  EXPLAIN (ANALYZE, PMU, VERBOSE, COSTS off, TIMING off, SUMMARY off,
           FORMAT YAML)
  SELECT count(*) FROM small$$);

-- The execution is the coordinator's part and the slices' together; each
-- slice of the segments has a QE on each primary segment, and the most
-- instructions of one QE are a part of its slice's.
CREATE TABLE plans AS
SELECT pmu_json($$
  EXPLAIN (ANALYZE, PMU, VERBOSE, SUMMARY off, FORMAT JSON)
  SELECT count(*) FROM small WHERE a > (SELECT avg(a) FROM small)$$) j
DISTRIBUTED RANDOMLY;
SELECT (j ->> 'Execution Instructions')::numeric =
       (j ->> 'Coordinator Instructions')::numeric +
       (SELECT sum((s ->> 'Instructions')::numeric)
        FROM jsonb_array_elements(j -> 'Slices PMU') s) AS instructions_add_up,
       (j ->> 'Execution Cycles')::numeric =
       (j ->> 'Coordinator Cycles')::numeric +
       (SELECT sum((s ->> 'Cycles')::numeric)
        FROM jsonb_array_elements(j -> 'Slices PMU') s) AS cycles_add_up
FROM plans;
SELECT (s ->> 'Slice')::int AS slice,
       (s ->> 'QEs')::int = (SELECT count(*) FROM gp_segment_configuration
                             WHERE content >= 0 AND role = 'p') AS qe_each,
       (s ->> 'Instructions')::numeric > 0 AS counted,
       (s ->> 'Cycles')::numeric > 0 AS cycles,
       (s ->> 'Max Instructions')::numeric > 0 AND
       (s ->> 'Max Instructions')::numeric
       <= (s ->> 'Instructions')::numeric AS max_within
FROM plans, jsonb_array_elements(j -> 'Slices PMU') s
ORDER BY 1;
SELECT k, (j ->> k)::numeric > 0 AS positive
FROM plans,
     unnest(ARRAY['Planning Instructions', 'Planning Cycles',
                  'Execution Instructions', 'Execution Cycles',
                  'Coordinator Instructions', 'Coordinator Cycles']) k;

-- The coordinator's part is its own work: large for a query that runs
-- on the coordinator, beside a plan that does almost nothing there.
WITH c AS (SELECT pmu_json($$EXPLAIN (ANALYZE, PMU, VERBOSE, SUMMARY off, FORMAT JSON)
                             SELECT count(*) FROM generate_series(1, 100000)$$) j),
     o AS (SELECT pmu_json($$EXPLAIN (ANALYZE, PMU, VERBOSE, SUMMARY off, FORMAT JSON)
                             SELECT 1$$) j)
SELECT (c.j ->> 'Coordinator Instructions')::numeric
       = (c.j ->> 'Execution Instructions')::numeric AS all_there,
       (c.j ->> 'Coordinator Instructions')::numeric
       > 10 * (o.j ->> 'Coordinator Instructions')::numeric AS its_work
FROM c, o;

-- An EXPLAIN inside the statement, with PMU or with ANALYZE alone, gives
-- the counts of its QEs to the statement: "Nested QEs".
CREATE FUNCTION inner_explain(options text) RETURNS bigint
LANGUAGE plpgsql AS $$
DECLARE
  doc text;
BEGIN
  EXECUTE format('EXPLAIN (%s, FORMAT JSON) SELECT count(*) FROM big', options)
    INTO doc;
  RETURN 1;
END
$$;
-- Their counts are those of the same EXPLAIN alone, not twice them. An
-- EXPLAIN without ANALYZE runs nothing on the segments, and adds
-- nothing.
WITH alone AS (
  SELECT (j ->> 'Execution Instructions')::numeric
         - (j ->> 'Coordinator Instructions')::numeric AS qes
  FROM pmu_json($$EXPLAIN (ANALYZE, PMU, VERBOSE, SUMMARY off, FORMAT JSON)
                  SELECT count(*) FROM big$$) j)
SELECT o AS options,
       j ? 'Nested QEs Instructions' AS nested_shown,
       (j ->> 'Nested QEs Instructions')::numeric
       BETWEEN alone.qes / 2 AND alone.qes * 3 / 2 AS as_alone
FROM alone, unnest(ARRAY['ANALYZE, PMU', 'ANALYZE', 'COSTS off']) o,
     pmu_json(format($$EXPLAIN (ANALYZE, PMU, VERBOSE, SUMMARY off, FORMAT JSON)
                       SELECT inner_explain(%L)$$, o)) j;

-- The arguments of EXECUTE are computed before its plan runs: an
-- EXPLAIN in them is no part of the plan's execution.
PREPARE one(bigint) AS SELECT $1;
SELECT j ? 'Nested QEs Instructions' AS nested_shown
FROM pmu_json($$EXPLAIN (ANALYZE, PMU, VERBOSE, SUMMARY off, FORMAT JSON)
                EXECUTE one(inner_explain('ANALYZE, PMU'))$$) j;
DEALLOCATE one;

-- An EXPLAIN ANALYZE inside that fails on the segments loses the counts
-- of its QEs, even when a function catches the error.
CREATE FUNCTION caught() RETURNS text
LANGUAGE plpgsql AS $$
BEGIN
  EXECUTE 'EXPLAIN ANALYZE SELECT count(*) FROM big WHERE 1 / (a - a) = 1';
  RETURN 'not caught';
EXCEPTION
  WHEN division_by_zero THEN
    RETURN 'caught';
END
$$;
SELECT caught();
EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, SUMMARY off)
SELECT caught();

-- A query inside the statement that runs on the segments without
-- EXPLAIN ANALYZE sends no counts, and the statement fails.
CREATE FUNCTION count_big() RETURNS bigint
LANGUAGE plpgsql AS $$
BEGIN
  RETURN (SELECT count(*) FROM big);
END
$$;
EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, SUMMARY off)
SELECT count_big();

-- So does a fetch inside from a cursor opened before whose QEs may
-- still run: they started at DECLARE, and the fetch makes them go on.
-- A cursor on the coordinator, one read to its end, and one whose only
-- work on the segments is an initplan, done at DECLARE, count.
CREATE FUNCTION fetch_from(name text) RETURNS int
LANGUAGE plpgsql AS $$
DECLARE
  c refcursor := name;
  v int;
BEGIN
  FETCH c INTO v;
  RETURN v;
END
$$;
BEGIN;
DECLARE on_coordinator CURSOR FOR SELECT g FROM generate_series(1, 3) g;
DECLARE read_out CURSOR FOR SELECT a FROM big;
MOVE ALL FROM read_out;
DECLARE initplan_only CURSOR FOR SELECT (SELECT count(*)::int FROM big);
SELECT c AS cursor, count(*) FILTER (WHERE line LIKE 'Execution PMU:%') AS counted
FROM unnest(ARRAY['on_coordinator', 'read_out', 'initplan_only']) c,
     pmu_explain(format($$EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, SUMMARY off)
                          SELECT fetch_from(%L)$$, c)) line
GROUP BY c ORDER BY c;
-- FETCH RELATIVE 0 before the first row runs the executor without
-- moving, and the QEs do nothing for it.
CREATE FUNCTION refetch(name text) RETURNS int
LANGUAGE plpgsql AS $$
DECLARE
  c refcursor := name;
  v int;
BEGIN
  FETCH RELATIVE 0 FROM c INTO v;
  RETURN v;
END
$$;
DECLARE not_read CURSOR FOR SELECT a FROM big;
SELECT count(*) FILTER (WHERE line LIKE 'Execution PMU:%') AS counted
FROM pmu_explain($$EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, SUMMARY off)
                   SELECT refetch('not_read')$$) line;
DECLARE on_segments CURSOR FOR SELECT a FROM big;
EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, SUMMARY off)
SELECT fetch_from('on_segments');
ROLLBACK;

-- More rows on the segments, more instructions there.
WITH s AS (SELECT pmu_json($$EXPLAIN (ANALYZE, PMU, VERBOSE, SUMMARY off, FORMAT JSON)
                             SELECT count(*) FROM small$$) j),
     b AS (SELECT pmu_json($$EXPLAIN (ANALYZE, PMU, VERBOSE, SUMMARY off, FORMAT JSON)
                             SELECT count(*) FROM big$$) j)
SELECT (b.j -> 'Slices PMU' -> 0 ->> 'Instructions')::numeric
       > (s.j -> 'Slices PMU' -> 0 ->> 'Instructions')::numeric AS larger
FROM s, b;

DROP TABLE small, big, copy, plans, nodata;
DROP FUNCTION pmu_explain(text), pmu_json(text), inner_explain(text),
  count_big(), caught(), fetch_from(text);
