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
# builds the version that cannot count; "make install" takes the same
# setting as "make". Each version has a target directory of its own,
# and the file .pmu_counters names the one the module is linked with.
CRATE = crates/pg_pmu_core
CARGO ?= cargo
PMU_COUNTERS = $(if $(filter none,$(PG_PMU_COUNTERS)),none,system)
RUST_TARGET = $(CRATE)/target/$(PMU_COUNTERS)
RUST_LIB = $(RUST_TARGET)/release/libpg_pmu_core.a
CARGO_FLAGS = --release --manifest-path $(CRATE)/Cargo.toml \
	--target-dir $(RUST_TARGET)
ifeq ($(PMU_COUNTERS),none)
CARGO_FLAGS += --features none
endif
RUST_SOURCES = $(shell find $(CRATE)/src -name '*.rs') \
	$(CRATE)/Cargo.toml $(CRATE)/Cargo.lock
SHLIB_LINK += $(RUST_LIB)
# The crate's target directory is kept: "cargo clean" in the crate.
EXTRA_CLEAN = output_unloaded tmp_check_unloaded .pmu_counters

# The suites with a second expected output for a system that cannot
# count; "make installcheck-counted" demands the first.
COUNTED_SUITES = counters

REGRESS = option counters
REGRESS_OPTS = --temp-config=$(srcdir)/pg_pmu.conf

PG_CONFIG ?= pg_config
PGXS := $(shell $(PG_CONFIG) --pgxs)
include $(PGXS)

$(shlib): $(RUST_LIB)

# A switch of the version removes the module while make reads this
# file, before it looks at any file's time: make on macOS compares
# times to the second, so a newer archive alone could miss a switch
# made within the same second.
ifneq ($(shell cat .pmu_counters 2>/dev/null),$(PMU_COUNTERS))
$(shell echo $(PMU_COUNTERS) > .pmu_counters; rm -f $(shlib))
endif

# Cargo runs only when a source of the crate has changed, so "make
# install" after "make" runs no Cargo, as root or otherwise. Cargo may
# keep the old archive; touch makes it newer than its sources.
$(RUST_LIB): $(RUST_SOURCES)
	$(CARGO) build $(CARGO_FLAGS)
	touch $@

installcheck: installcheck-unloaded

# On a machine that must count: the suites, and then each of the
# suites above must have matched its first expected output.
.PHONY: installcheck-counted
installcheck-counted: installcheck
	@for t in $(COUNTED_SUITES); do \
		cmp -s results/$$t.out $(srcdir)/expected/$$t.out || { \
			echo "pg_pmu: $$t matched the output of a system that cannot count"; \
			exit 1; }; \
	done

# The checks of the crate, and that its code in the archive calls no
# panic: a panic would abort the process of PostgreSQL.
.PHONY: check-crate
check-crate: $(RUST_LIB)
	cd $(CRATE) && $(CARGO) fmt --check && $(CARGO) clippy --release \
		&& $(CARGO) clippy --all-targets && $(CARGO) test
	@nm -u $(RUST_LIB) 2>/dev/null | awk \
		'/^[^ \t].*\.o\)?:$$/ { crate = ($$0 ~ /pg_pmu_core/); seen += crate } \
		 crate && /panicking|panic_bounds|panic_fmt/ { print; bad = 1 } \
		 END { if (!seen) { print "pg_pmu_core: no code of the crate in the archive"; exit 1 } \
		       if (bad) { print "pg_pmu_core: the crate can panic"; exit 1 } }'

.PHONY: installcheck-unloaded
installcheck-unloaded:
	$(pg_regress_installcheck) \
		--temp-instance=$(CURDIR)/tmp_check_unloaded \
		--outputdir=$(CURDIR)/output_unloaded \
		unloaded
