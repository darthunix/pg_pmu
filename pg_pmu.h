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

/*
 * A cell of the shared memory that a counting statement holds for its
 * workers. Holds outlive their statements when these fail: a hold goes
 * back when nobody can still write to its cell.
 */
typedef struct PmuHold PmuHold;

/* In pg_pmu.c: turn a code of the crate into an error. */
pg_noreturn extern void pmu_report(int32 status, int32 os_error);

/* In workers.c. */
extern void pmu_workers_init(void);
extern PmuHold *pmu_take_cell(int *guc_level);
extern void pmu_return_cell(PmuHold *hold, int guc_level);

#endif							/* PG_PMU_H */
