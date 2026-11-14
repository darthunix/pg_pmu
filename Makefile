# pg_pmu: counts of instructions and cycles of the processor in
# EXPLAIN ANALYZE.
#
# The suites need temporary servers:
#
#   make installcheck EXTRA_REGRESS_OPTS="--temp-instance=$PWD/tmp_check"
#
# The suite "unloaded" needs a server without the module, so it runs
# first, on a temporary server of its own; the others run on one
# started with pg_pmu.conf.

MODULE_big = pg_pmu
OBJS = pg_pmu.o
PGFILEDESC = "pg_pmu - counts of the processor in EXPLAIN ANALYZE"

REGRESS = option
REGRESS_OPTS = --temp-config=$(srcdir)/pg_pmu.conf

PG_CONFIG ?= pg_config
PGXS := $(shell $(PG_CONFIG) --pgxs)
include $(PGXS)

installcheck: installcheck-unloaded

.PHONY: installcheck-unloaded
installcheck-unloaded:
	$(pg_regress_installcheck) \
		--temp-instance=$(CURDIR)/tmp_check_unloaded \
		--outputdir=$(CURDIR)/output_unloaded \
		unloaded
