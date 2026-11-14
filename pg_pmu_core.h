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
} PmuLayout;

extern void pmu_core_layout(PmuLayout *out);

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

#endif							/* PG_PMU_CORE_H */
