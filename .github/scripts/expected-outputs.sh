#!/bin/sh
# Say which expected output each suite of the last run matched:
#   expected-outputs.sh [<expected file of the suite counters it must match>]
# With an argument, fail unless the suite counters matched it.
for r in results/*.out output_unloaded/results/*.out; do
	[ -f "$r" ] || continue
	t=$(basename "$r" .out)
	matched=none
	for e in pg/expected/"$t".out pg/expected/"$t"_[0-9].out; do
		if [ -f "$e" ] && cmp -s "$r" "$e"; then
			matched=$(basename "$e")
			break
		fi
	done
	echo "$t matched $matched"
	if [ $# -gt 0 ] && [ "$t" = counters ] && [ "$matched" != "$1" ]; then
		echo "counters had to match $1" >&2
		exit 1
	fi
done
