-- On Linux with kernel.perf_event_paranoid set to 4, at which an Ubuntu
-- kernel refuses the events to every process without privileges, as CI
-- sets it on its Ubuntu runners: the error, the error of the system,
-- and the hint.
-- spec: explain-pmu/Linux, counting refused
EXPLAIN (ANALYZE, PMU) SELECT 1;
