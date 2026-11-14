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

#include "pg_pmu_core.h"

/*
 * A cell of the shared memory that a counting statement holds for its
 * workers. Holds outlive their statements when these fail: a hold goes
 * back when nobody can still write to its cell.
 */
typedef struct PmuHold PmuHold;

/* In pg_pmu.c: the codes of the crate as errors, and the counters. */
pg_noreturn extern void pmu_report(int32 status, int32 os_error);
extern int	pmu_status_sqlstate(int32 status);
extern void pmu_open_counters(void);

/* In workers.c: the cells, and the counting in a worker. */
extern void pmu_workers_init(void);
extern PmuHold *pmu_take_cell(int *guc_level);
extern void pmu_return_cell(PmuHold *hold, int guc_level);
extern void pmu_hold_read(PmuHold *hold, PmuCellContents *out);
extern void pmu_worker_start(QueryDesc *queryDesc);
extern void pmu_worker_finish(QueryDesc *queryDesc);

#endif							/* PG_PMU_H */
