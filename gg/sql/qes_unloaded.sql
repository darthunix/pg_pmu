-- The module on the coordinator only, where the QEs of an entry db load
-- it too: the QEs of the segments send their statistics without a
-- report, and the statement fails rather than print counts without them.
--start_ignore
\! gpconfig -c session_preload_libraries --coordinatoronly -v "$(psql -At -c "SELECT array_to_string(array_append(string_to_array(current_setting('session_preload_libraries'), ','), 'pg_pmu'), ',')" postgres)"
\! gpstop -u
\! sleep 1
--end_ignore
\c
SET optimizer = off;
CREATE TABLE unloaded (a int, b int) DISTRIBUTED BY (b);
INSERT INTO unloaded SELECT i, i FROM generate_series(1, 100) i;
EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, SUMMARY off)
SELECT count(*) FROM unloaded;
-- A slice on the entry db reports, those of the segments do not.
EXPLAIN (ANALYZE, PMU, COSTS off, TIMING off, SUMMARY off)
INSERT INTO unloaded SELECT 0, 0 FROM pg_am LIMIT 1;
DROP TABLE unloaded;
--start_ignore
\! v="$(psql -At -c "SELECT array_to_string(array_remove(string_to_array(current_setting('session_preload_libraries'), ','), 'pg_pmu'), ',')" postgres)"; if [ -z "$v" ]; then gpconfig -r session_preload_libraries --coordinatoronly; else gpconfig -c session_preload_libraries --coordinatoronly -v "$v"; fi
\! gpstop -u
\! sleep 1
--end_ignore
