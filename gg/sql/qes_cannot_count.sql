-- With the build PG_PMU_TEST=qes-cannot-count no QE can count: each says
-- so in its report, and the coordinator fails the statement with the
-- code of the report.
SET optimizer = off;
CREATE TABLE uncounted (a int) DISTRIBUTED BY (a);
INSERT INTO uncounted SELECT generate_series(1, 100);
\set VERBOSITY sqlstate
EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, SUMMARY off)
SELECT count(*) FROM uncounted;
\set VERBOSITY default
EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, SUMMARY off)
SELECT count(*) FROM uncounted;
-- A function that catches the error of an EXPLAIN inside does not hide
-- it: the counts of the inner statement's QEs are lost, and the
-- statement fails.
CREATE FUNCTION caught() RETURNS text
LANGUAGE plpgsql AS $$
BEGIN
  EXECUTE 'EXPLAIN (ANALYZE, PMU) SELECT count(*) FROM uncounted';
  RETURN 'not caught';
EXCEPTION
  WHEN OTHERS THEN
    RETURN 'caught';
END
$$;
SELECT caught();
EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, SUMMARY off)
SELECT caught();
DROP FUNCTION caught();
-- A statement without the option is not affected.
EXPLAIN (ANALYZE, COSTS off, TIMING off, SUMMARY off)
SELECT count(*) FROM uncounted;
DROP TABLE uncounted;
