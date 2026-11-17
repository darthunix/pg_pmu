#!/bin/sh
# Say what the runner is, and whether its kernel shows a hardware PMU:
# a unit "cpu" of perf events, as on x86-64, or units with a list of
# processors, as on AArch64 and on hybrid processors. Writes COUNTS=true
# to $GITHUB_ENV when it does, so that the job demands counts, and
# COUNTS=false when it does not, as on some x86-64 runners, whose hosts
# give their virtual machines no PMU. EVENTS=true says the job demands
# the branches and the cache too: on the runners of AArch64, whose PMU
# counts all of them at once. The x86-64 runners that count are virtual
# machines whose hosts may hide the events of the cache.
set -eu
uname -sr
counts=false
if [ "$(uname -s)" = Linux ]; then
	lscpu | grep -E '^(Vendor ID|Model name|CPU\(s\)):' || true
	devices=/sys/bus/event_source/devices
	echo "units of perf events: $(ls "$devices" | tr '\n' ' ')"
	if [ -d "$devices/cpu" ] || ls "$devices"/*/cpus > /dev/null 2>&1; then
		counts=true
	fi
else
	sysctl -n machdep.cpu.brand_string
fi
events=false
if [ "$counts" = true ] && [ "$(uname -m)" = aarch64 ]; then
	events=true
fi
echo "a hardware PMU: $counts; the branches and the cache demanded: $events"
echo "COUNTS=$counts" >> "${GITHUB_ENV:-/dev/null}"
echo "EVENTS=$events" >> "${GITHUB_ENV:-/dev/null}"
