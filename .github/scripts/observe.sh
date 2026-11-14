#!/bin/sh
# The observations of the README: ten runs of each statement in one
# session after a run that fills the caches, against the server psql
# reaches by its environment, with Tessera and pg_pmu loaded and a
# table t of 100000 rows; then the smallest count of each and how much
# the largest exceeds it.
set -eu
run() {
  for i in $(seq 11); do echo "$1"; done |
    psql -XAt | grep 'Execution PMU' | tail -10
}
{
  echo "== SELECT 1"
  run "EXPLAIN (ANALYZE, PMU, TIMING off) SELECT 1;"
  echo "== count(*) with Tessera"
  run "EXPLAIN (ANALYZE, PMU, TIMING off) SELECT count(*) FROM t;"
  echo "== count(*) with PostgreSQL's own plan"
  run "SET tessera.enable = off;
     EXPLAIN (ANALYZE, PMU, TIMING off) SELECT count(*) FROM t;"
} | tee observations.txt
awk '
  /^==/ { name = substr($0, 4); names[++n] = name; next }
  {
    split($3, a, "="); split($4, b, "=")
    i = a[2] + 0; c = b[2] + 0
    if (!(name in imin) || i < imin[name]) imin[name] = i
    if (i > imax[name]) imax[name] = i
    if (!(name in cmin) || c < cmin[name]) cmin[name] = c
    if (c > cmax[name]) cmax[name] = c
  }
  END {
    for (k = 1; k <= n; k++) {
      m = names[k]
      printf "%s: %d instructions (%.4f %%), %d cycles (%.1f %%)\n", m,
        imin[m], (imax[m] - imin[m]) * 100 / imin[m],
        cmin[m], (cmax[m] - cmin[m]) * 100 / cmin[m]
    }
  }' observations.txt
