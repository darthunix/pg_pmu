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
OBJS = pg_pmu.o test/abi.o
PGFILEDESC = "pg_pmu - counts of the processor in EXPLAIN ANALYZE"

# The Rust crate, linked in as a static library. PG_PMU_COUNTERS=none
# builds the version that cannot count.
CRATE = crates/pg_pmu_core
CARGO ?= cargo
RUST_LIB = $(CRATE)/target/release/libpg_pmu_core.a
CARGO_FLAGS = --release --manifest-path $(CRATE)/Cargo.toml
ifeq ($(PG_PMU_COUNTERS),none)
CARGO_FLAGS += --features none
endif
SHLIB_LINK += $(RUST_LIB)
EXTRA_CLEAN = output_unloaded

REGRESS = option counters
REGRESS_OPTS = --temp-config=$(srcdir)/pg_pmu.conf

PG_CONFIG ?= pg_config
PGXS := $(shell $(PG_CONFIG) --pgxs)
include $(PGXS)

# Cargo decides whether the crate must be built again; the module is
# linked again each time, so a switch to or from PG_PMU_COUNTERS=none
# always takes effect.
$(shlib): $(RUST_LIB)

.PHONY: FORCE
$(RUST_LIB): FORCE
	$(CARGO) build $(CARGO_FLAGS)

installcheck: installcheck-unloaded

.PHONY: installcheck-unloaded
installcheck-unloaded:
	$(pg_regress_installcheck) \
		--temp-instance=$(CURDIR)/tmp_check_unloaded \
		--outputdir=$(CURDIR)/output_unloaded \
		unloaded
