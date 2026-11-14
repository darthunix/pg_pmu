/*-------------------------------------------------------------------------
 *
 * pg_pmu_core.h
 *		The functions of the Rust crate pg_pmu_core, which C calls.
 *
 * Written by hand. The crate's tests compare the codes below with its
 * own, and the module compares the layout of PmuCounts with the
 * crate's when it loads (pmu_check_layout in test/abi.c).
 *
 * Every function returns a code, PMU_OK or one of the others, and
 * writes the error of the system (errno) to os_error, 0 when there is
 * none. Nothing in the crate panics or keeps a pointer it was given.
 *
 * Copyright (c) 2026, Denis Smirnov
 *
 *-------------------------------------------------------------------------
 */
#ifndef PG_PMU_CORE_H
#define PG_PMU_CORE_H

#include <stddef.h>
#include <stdint.h>

/* The codes; the SQLSTATE each one becomes is in the comment. */
#define PMU_OK					0
#define PMU_NOT_PERMITTED		1	/* 42501 */
#define PMU_NO_SYSTEM			2	/* 0A000 */
#define PMU_NO_COUNTERS			3	/* 0A000 */
#define PMU_CORES_NOT_COVERED	4	/* 0A000 */
#define PMU_KIND_NOT_OPENABLE	5	/* 0A000 */
#define PMU_NO_FREE_COUNTER		6	/* 53000 */
#define PMU_NO_DESCRIPTOR		7	/* 53000 */
#define PMU_COUNTER_LOST		8	/* 53000 */
#define PMU_SYSTEM_ERROR		9	/* XX000 */
#define PMU_INTERNAL			10	/* XX000 */
#define PMU_TOO_MANY_KINDS		11	/* 0A000 */
#define PMU_STALE_TOKEN			12	/* XX000 */
#define PMU_CELL_BUSY			13	/* XX000 */
#define PMU_NO_CELL				14	/* 53000 */
#define PMU_BAD_TOKEN			15	/* XX000 */

/* Instructions and cycles counted so far; only differences mean much. */
typedef struct PmuCounts
{
	uint64_t	instructions;
	uint64_t	cycles;
} PmuCounts;

/* The crate's sizes and offsets of the types above. */
typedef struct PmuLayout
{
	uint32_t	counts_size;
	uint32_t	counts_cycles;
	uint32_t	perf_abi_size;
	uint32_t	contents_size;
	uint32_t	contents_failures;
} PmuLayout;

extern void pmu_core_layout(PmuLayout *out);

/*
 * What the crate puts in a struct perf_event_attr and passes to the
 * kernel; on Linux the module compares it with <linux/perf_event.h>
 * when it loads. The flags are the word of bit fields of the structure.
 */
typedef struct PmuPerfAbi
{
	uint32_t	attr_size;
	uint32_t	config_offset;
	uint32_t	read_format_offset;
	uint32_t	flags_offset;
	uint32_t	type_hardware;
	uint32_t	pmu_type_shift;
	uint64_t	hw_instructions;
	uint64_t	hw_cycles;
	uint64_t	format_group;
	uint64_t	leader_flags;
	uint64_t	member_flags;
	uint64_t	flag_fd_cloexec;
	uint64_t	ioc_enable;
	uint64_t	ioc_disable;
	uint64_t	ioc_flag_group;
} PmuPerfAbi;

extern void pmu_core_perf_abi(PmuPerfAbi *out);

/*
 * The counters of this process. Plan, take the descriptors it asks
 * for from PostgreSQL's budget, open, and give the descriptors back if
 * the opening fails. Then each counting statement or execution of a
 * worker acquires, reads, and releases; the counters count while a
 * reference is held. sysfs is the root of sysfs, "/sys".
 */
extern int32_t pmu_counters_plan(const char *sysfs, uint32_t *descriptors,
								 int32_t *os_error);
extern int32_t pmu_counters_open(int32_t *os_error);
extern int32_t pmu_counters_acquire(PmuCounts *now, int32_t *os_error);
extern int32_t pmu_counters_read(PmuCounts *now, int32_t *os_error);
extern int32_t pmu_counters_release(int32_t *os_error);

/*
 * The cells of the memory "pg_pmu", where the parallel workers of a
 * counting statement report; the layout and the token are in the spec.
 * The module allocates the memory and passes its address to each call.
 * These functions do not write os_error.
 */

/* What a cell held when it was given back. */
typedef struct PmuCellContents
{
	uint64_t	instructions;
	uint64_t	cycles;
	uint32_t	failures;		/* workers that could not count */
	uint32_t	sqlstate;		/* of the last of them */
} PmuCellContents;

/*
 * The bytes of the memory for n cells of the backends and k of the pool,
 * or 0 when their numbers do not fit in a token.
 */
extern size_t pmu_cells_size(uint32_t n, uint32_t k);

/* Write the header and free cells, once, before any process uses them. */
extern int32_t pmu_cells_init(void *base, uint32_t n, uint32_t k);

/*
 * Take the cell of backend index, or a cell of the pool (PMU_NO_CELL when
 * none is free), and write its token.
 */
extern int32_t pmu_cell_take(const void *base, uint32_t index,
							 uint64_t *token);
extern int32_t pmu_cell_take_pool(const void *base, uint64_t *token);

/*
 * Give the cell of token back and write what it held. PMU_BAD_TOKEN,
 * PMU_STALE_TOKEN and PMU_CELL_BUSY refuse it and change nothing.
 */
extern int32_t pmu_cell_give_back(const void *base, uint64_t token,
								  PmuCellContents *out);

#endif							/* PG_PMU_CORE_H */
