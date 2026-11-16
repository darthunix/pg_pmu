/*-------------------------------------------------------------------------
 *
 * report.h
 *		The codes of the crate as errors of the core.
 *
 * Copyright (c) 2026, Denis Smirnov
 *
 *-------------------------------------------------------------------------
 */
#ifndef PMU_REPORT_H
#define PMU_REPORT_H

#include "pmu_counters.h"

/* Raise the error of a code of the crate, with the error of the system. */
#ifdef pg_noreturn
pg_noreturn extern void pmu_report(int32 status, int32 os_error);
#else
extern void pmu_report(int32 status, int32 os_error) pg_attribute_noreturn();
#endif

/* The SQLSTATE of a code of the crate. */
extern int	pmu_status_sqlstate(int32 status);

/*
 * The detail of the error of the system, where the crate gave one: for
 * an ereport where errno and os_error hold it.
 */
#define PMU_SYSTEM_DETAIL() \
	(os_error != 0 ? errdetail("The system said: %m.") : 0)

#endif							/* PMU_REPORT_H */
