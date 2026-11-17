/*-------------------------------------------------------------------------
 *
 * pg_pmu.h
 *		What the files of the module share.
 *
 * Copyright (c) 2026, Denis Smirnov
 *
 *-------------------------------------------------------------------------
 */
#ifndef PG_PMU_H
#define PG_PMU_H

#include "executor/execdesc.h"

#include "pmu_counters.h"

/*
 * A cell of the shared memory that a counting statement holds for its
 * workers. Holds outlive their statements when these fail: a hold goes
 * back when nobody can still write to its cell.
 */
typedef struct PmuHold PmuHold;

/* In pg_pmu.c: the codes of the crate as errors, and the counters. */
pg_noreturn extern void pmu_report(int32 status, int32 os_error);
extern int	pmu_status_sqlstate(int32 status);
extern void pmu_open_counters(uint32 sets);

/* In workers.c: the cells, and the counting in a worker. */
extern void pmu_workers_init(void);
extern PmuHold *pmu_take_cell(uint32 sets, int *guc_level);
extern void pmu_return_cell(PmuHold *hold, int guc_level);
extern void pmu_hold_read(PmuHold *hold, PmuCellContents *out);
extern void pmu_worker_start(QueryDesc *queryDesc);
extern void pmu_worker_finish(QueryDesc *queryDesc);

/* a - b, value by value: the counts between two readings. */
static inline PmuCounts
pmu_counts_minus(const PmuCounts *a, const PmuCounts *b)
{
	PmuCounts	d;

	d.instructions = a->instructions - b->instructions;
	d.cycles = a->cycles - b->cycles;
	d.branches = a->branches - b->branches;
	d.branch_misses = a->branch_misses - b->branch_misses;
	d.cache_references = a->cache_references - b->cache_references;
	d.cache_misses = a->cache_misses - b->cache_misses;
	return d;
}

/* Add b to a, value by value. */
static inline void
pmu_counts_add(PmuCounts *a, const PmuCounts *b)
{
	a->instructions += b->instructions;
	a->cycles += b->cycles;
	a->branches += b->branches;
	a->branch_misses += b->branch_misses;
	a->cache_references += b->cache_references;
	a->cache_misses += b->cache_misses;
}

#endif							/* PG_PMU_H */
