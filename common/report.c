/*-------------------------------------------------------------------------
 *
 * report.c
 *		The codes of the crate as errors of the core: the same on every
 *		core pg_pmu builds for.
 *
 * Copyright (c) 2026, Denis Smirnov
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#include <errno.h>

#include "report.h"

/*
 * The SQLSTATE of a code of the crate, as the README lists it.
 */
int
pmu_status_sqlstate(int32 status)
{
	switch (status)
	{
		case PMU_NOT_PERMITTED:
			return ERRCODE_INSUFFICIENT_PRIVILEGE;
		case PMU_NO_SYSTEM:
		case PMU_NO_COUNTERS:
		case PMU_CORES_NOT_COVERED:
		case PMU_KIND_NOT_OPENABLE:
		case PMU_TOO_MANY_KINDS:
		case PMU_TOO_MANY_THREADS:
		case PMU_EVENTS_NOT_COUNTED:
			return ERRCODE_FEATURE_NOT_SUPPORTED;
		case PMU_NO_FREE_COUNTER:
		case PMU_NO_DESCRIPTOR:
		case PMU_COUNTER_LOST:
		case PMU_NO_CELL:
			return ERRCODE_INSUFFICIENT_RESOURCES;
		default:
			return ERRCODE_INTERNAL_ERROR;
	}
}

/*
 * Turn a code of the crate into an error of PostgreSQL.
 */
void
pmu_report(int32 status, int32 os_error)
{
	errno = os_error;

	switch (status)
	{
		case PMU_NOT_PERMITTED:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("the system does not allow this process to count instructions"),
					 PMU_SYSTEM_DETAIL(),
					 errhint("On Linux, set kernel.perf_event_paranoid to 2 or less, "
							 "or give the server's processes CAP_PERFMON; "
							 "in a container, its filter of system calls may refuse "
							 "perf_event_open too.")));
			break;
		case PMU_NO_SYSTEM:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("pg_pmu cannot count on this system"),
					 errdetail("It counts on Linux and macOS, and this build of it "
							   "has no counters.")));
			break;
		case PMU_NO_COUNTERS:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("the system keeps no counters of instructions for this process"),
					 errdetail("A virtual machine often gives none.")));
			break;
		case PMU_CORES_NOT_COVERED:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("the kinds of cores in sysfs do not cover all the processors online"),
					 errdetail("Counting on some cores only would show a part of the work.")));
			break;
		case PMU_TOO_MANY_KINDS:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("the processor has more kinds of cores than pg_pmu counts on"),
					 errdetail("It counts on up to four kinds.")));
			break;
		case PMU_TOO_MANY_THREADS:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("this process runs more threads than pg_pmu counts on"),
					 errdetail("It counts up to 64 threads that run when the counters open.")));
			break;
		case PMU_EVENTS_NOT_COUNTED:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("this system cannot count the branches or the cache"),
					 PMU_SYSTEM_DETAIL()));
			break;
		case PMU_KIND_NOT_OPENABLE:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("the kernel cannot count on every kind of cores of this processor"),
					 PMU_SYSTEM_DETAIL(),
					 errhint("Counting on several kinds of cores needs Linux 5.13 or later on Intel, and a kernel with extended hardware types on Arm.")));
			break;
		case PMU_NO_FREE_COUNTER:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("no hardware counter is free for this process"),
					 errhint("Each pair of events counted takes two of the few counters of a core, "
							 "and other users of them, such as the NMI watchdog of Linux, take some; "
							 "the branches and the cache may be counted in separate statements.")));
			break;
		case PMU_NO_DESCRIPTOR:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("no file descriptor is left for the counters"),
					 PMU_SYSTEM_DETAIL()));
			break;
		case PMU_COUNTER_LOST:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("the system took a counter away while the statement ran")));
			break;
		case PMU_SYSTEM_ERROR:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("the counters of the processor failed"),
					 PMU_SYSTEM_DETAIL()));
			break;
		case PMU_NO_CELL:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("too many nested counting statements run at once"),
					 errhint("Raise pg_pmu.max_nested.")));
			break;
		case PMU_STALE_TOKEN:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("internal error of pg_pmu: the cell of the workers was given back")));
			break;
		case PMU_CELL_BUSY:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("internal error of pg_pmu: a worker still adds to the cell")));
			break;
		case PMU_BAD_TOKEN:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("internal error of pg_pmu: the token of the workers is not valid")));
			break;
		default:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("internal error of pg_pmu: code %d", status)));
			break;
	}
	pg_unreachable();
}
