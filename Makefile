# pg_pmu: counts of instructions and cycles of the processor in
# EXPLAIN ANALYZE.
#
# The C code of the module for PostgreSQL, its suites and their settings
# are in pg/; the Rust crate and the check of its header are shared.
#
# The suites need temporary servers:
#
#   make installcheck EXTRA_REGRESS_OPTS="--temp-instance=$PWD/tmp_check"
#
# The suite "unloaded" needs a server without the module, so it runs
# first, on a temporary server of its own; the others run on one
# started with pg/pg_pmu.conf.

MODULE_big = pg_pmu
OBJS = pg/pg_pmu.o pg/workers.o test/abi.o
PGFILEDESC = "pg_pmu - counts of the processor in EXPLAIN ANALYZE"

# The Rust crate, linked in as a static library. PG_PMU_COUNTERS=none
# builds the version that cannot count; "make install" takes the same
# setting as "make". Each version has a target directory of its own,
# and the file .pmu_variant names the one the module is linked with.
CRATE = crates/pmu_counters
CARGO ?= cargo
PMU_COUNTERS = $(if $(filter none,$(PG_PMU_COUNTERS)),none,system)
RUST_TARGET = $(CRATE)/target/$(PMU_COUNTERS)
RUST_LIB = $(RUST_TARGET)/release/libpmu_counters.a
CARGO_FLAGS = --release --manifest-path $(CRATE)/Cargo.toml \
	--target-dir $(RUST_TARGET)
ifeq ($(PMU_COUNTERS),none)
CARGO_FLAGS += --features none
endif
RUST_SOURCES = $(shell find $(CRATE)/src -name '*.rs') \
	$(CRATE)/Cargo.toml $(CRATE)/Cargo.lock
SHLIB_LINK += $(RUST_LIB)
# The crate's target directory is kept: "cargo clean" in the crate.
EXTRA_CLEAN = output_unloaded tmp_check_unloaded .pmu_variant

# PG_PMU_TEST=workers-cannot-count builds a module whose parallel
# workers fail to open their counters, for the suite
# workers_cannot_count, which runs only on a machine that counts.
ifeq ($(PG_PMU_TEST),workers-cannot-count)
PG_CPPFLAGS += -DPMU_TEST_WORKERS_CANNOT_COUNT
else ifneq ($(PG_PMU_TEST),)
$(error PG_PMU_TEST is workers-cannot-count or empty)
endif
PMU_VARIANT = $(PMU_COUNTERS)$(if $(PG_PMU_TEST), $(PG_PMU_TEST))

# The suites with a second expected output for a system that cannot
# count; "make installcheck-counted" demands the first. The suites of
# the branches and the cache demand it too with PG_PMU_REQUIRE_EVENTS,
# on a machine that counts them all at once: macOS never does, and
# virtual machines may hide them.
COUNTED_SUITES = counters
ifneq ($(PG_PMU_REQUIRE_EVENTS),)
COUNTED_SUITES += events_branches events_cache events_full
endif

REGRESS = option counters events_branches events_cache events_full
REGRESS_OPTS = --inputdir=$(srcdir)/pg --temp-config=$(srcdir)/pg/pg_pmu.conf

PG_CONFIG ?= pg_config
PGXS := $(shell $(PG_CONFIG) --pgxs)
include $(PGXS)

ifeq ($(PORTNAME),linux)
# Keep the crate's symbols inside the module: PostgreSQL loads modules
# with RTLD_GLOBAL, and another module written in Rust may define the
# same ones.
SHLIB_LINK += -Wl,--exclude-libs,libpmu_counters.a
endif

$(shlib): $(RUST_LIB)

# A switch of the version removes the module while make reads this
# file, before it looks at any file's time: make on macOS compares
# times to the second, so a newer archive alone could miss a switch
# made within the same second. Only a build switches: "make install"
# with other settings than the "make" before it stops rather than build
# another version, perhaps as root, and the other targets leave the
# module as it is.
PMU_BUILT = $(shell cat .pmu_variant 2>/dev/null)
ifneq ($(PMU_BUILT),$(PMU_VARIANT))
ifneq ($(PMU_BUILT),)
ifneq ($(filter install,$(MAKECMDGOALS)),)
$(error pg_pmu was built as "$(PMU_BUILT)"; give "make install" the PG_PMU_COUNTERS and PG_PMU_TEST of "make")
endif
endif
ifneq ($(if $(MAKECMDGOALS),$(filter all install,$(MAKECMDGOALS)),all),)
$(shell echo $(PMU_VARIANT) > .pmu_variant; rm -f $(shlib) pg/workers.o)
endif
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
		cmp -s results/$$t.out $(srcdir)/pg/expected/$$t.out || { \
			echo "pg_pmu: $$t matched the output of a system that cannot count"; \
			exit 1; }; \
	done

# The checks of the crate, and that its code in the archive calls no
# Rust code outside it: no panic, which would abort the process of
# PostgreSQL, and no function of core left out of line, which would
# need a personality of unwinding that the archive does not have. Only
# the system's functions may be undefined; a Rust function that one
# object of the crate calls and another defines is the crate's own.
.PHONY: check-crate
check-crate: $(RUST_LIB)
	cd $(CRATE) && $(CARGO) fmt --check \
		&& $(CARGO) clippy --release -- -D warnings \
		&& $(CARGO) clippy --all-targets -- -D warnings && $(CARGO) test
	@nm $(RUST_LIB) 2>/dev/null | awk \
		'/^[^ \t].*\.o\)?:$$/ { crate = ($$0 ~ /pmu_counters/); seen += crate; next } \
		 crate && $$NF ~ /^_?(_R[A-Za-z0-9]|_ZN|rust_eh_personality|_Unwind_)/ { \
		       if ($$(NF-1) == "U") undefined[$$NF] = 1; else defined[$$NF] = 1 } \
		 END { if (!seen) { print "pmu_counters: no code of the crate in the archive"; exit 1 } \
		       for (s in undefined) if (!(s in defined)) { print s; bad = 1 } \
		       if (bad) { print "pmu_counters: the crate calls Rust code outside it"; exit 1 } }'

# Suites of systems that cannot count, which CI makes: Linux with
# kernel.perf_event_paranoid set to 4, and a virtual machine with macOS.
.PHONY: installcheck-paranoid installcheck-vm
installcheck-paranoid:
	$(pg_regress_installcheck) $(REGRESS_OPTS) paranoid
installcheck-vm:
	$(pg_regress_installcheck) $(REGRESS_OPTS) vm

# With the build PG_PMU_TEST=workers-cannot-count, on a machine that
# counts: a function catches the error of a worker that cannot count.
.PHONY: installcheck-workers-cannot-count
installcheck-workers-cannot-count:
	$(pg_regress_installcheck) $(REGRESS_OPTS) workers_cannot_count

.PHONY: installcheck-unloaded
installcheck-unloaded:
	$(pg_regress_installcheck) --inputdir=$(srcdir)/pg \
		--temp-instance=$(CURDIR)/tmp_check_unloaded \
		--outputdir=$(CURDIR)/output_unloaded \
		unloaded
