/*-------------------------------------------------------------------------
 *
 * pg_pmu.c
 *		Counts of instructions and cycles of the processor in EXPLAIN
 *		ANALYZE.
 *
 * The module adds the option PMU to EXPLAIN. It must be loaded with
 * shared_preload_libraries: it defines a setting of the start of the
 * server and requests shared memory then.
 *
 * Copyright (c) 2026, Denis Smirnov
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#include <errno.h>

#include "commands/defrem.h"
#include "commands/explain_state.h"
#include "fmgr.h"
#include "miscadmin.h"
#include "storage/fd.h"
#include "utils/guc.h"

#include "pg_pmu_core.h"

PG_MODULE_MAGIC_EXT(.name = "pg_pmu",.version = "0.1.0");

/* The root of sysfs, where Linux lists the kinds of cores. */
#define PMU_SYSFS "/sys"

/* In test/abi.c. */
extern void pmu_check_layout(void);

/*
 * What the module keeps in the state of one EXPLAIN. It lives in the
 * memory of that EXPLAIN and dies with it.
 */
typedef struct PmuExplainState
{
	bool		pmu;			/* the option PMU is on */
} PmuExplainState;

/* pg_pmu.max_nested: nested counting statements the server runs at once */
static int	pmu_max_nested = 16;

/* Our slot in the extension state of an EXPLAIN. */
static int	pmu_extension_id;

/* The counters of this process are open; they stay so until it exits. */
static bool pmu_counters_opened = false;

static explain_validate_options_hook_type prev_validate_options_hook = NULL;

pg_noreturn static void pmu_report(int32 status, int32 os_error);
static void pmu_release_descriptors(uint32 n);
static void pmu_open_counters(void);
static PmuExplainState *pmu_ensure_state(ExplainState *es);
static void pmu_option_handler(ExplainState *es, DefElem *opt,
							   ParseState *pstate);
static bool pmu_option_guc_check(const char *option_name,
								 const char *option_value,
								 NodeTag option_type);
static void pmu_validate_options(ExplainState *es, List *options,
								 ParseState *pstate);

void
_PG_init(void)
{
	/*
	 * Everything below needs the start of the server: a setting of the
	 * start, and shared memory. PostgreSQL fails both later without a
	 * useful message, so this check comes first. A failed initialization
	 * leaves the library off the list of loaded ones, so every later LOAD
	 * fails the same way.
	 */
	if (!process_shared_preload_libraries_in_progress)
		ereport(ERROR,
				(errcode(ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE),
				 errmsg("pg_pmu must be loaded with shared_preload_libraries")));

	/* The header written by hand must match the crate. */
	pmu_check_layout();

	DefineCustomIntVariable("pg_pmu.max_nested",
							"Nested counting statements the server can run at once.",
							"A counting EXPLAIN that starts while another one of "
							"the same backend runs takes one of them.",
							&pmu_max_nested,
							16, 0, 1024,
							PGC_POSTMASTER,
							0,
							NULL, NULL, NULL);
	MarkGUCPrefixReserved("pg_pmu");

	pmu_extension_id = GetExplainExtensionId("pg_pmu");
	RegisterExtensionExplainOption("pmu", pmu_option_handler,
								   pmu_option_guc_check);

	prev_validate_options_hook = explain_validate_options_hook;
	explain_validate_options_hook = pmu_validate_options;
}

/*
 * Return the module's state of this EXPLAIN, making it if needed.
 */
static PmuExplainState *
pmu_ensure_state(ExplainState *es)
{
	PmuExplainState *state = GetExplainExtensionState(es, pmu_extension_id);

	if (state == NULL)
	{
		state = palloc0_object(PmuExplainState);
		SetExplainExtensionState(es, pmu_extension_id, state);
	}
	return state;
}

/*
 * EXPLAIN (PMU [boolean]): takes the values of PostgreSQL's own boolean
 * options.
 */
static void
pmu_option_handler(ExplainState *es, DefElem *opt, ParseState *pstate)
{
	pmu_ensure_state(es)->pmu = defGetBoolean(opt);
}

/*
 * The check PostgreSQL asks of every option of an extension, for
 * auto_explain.log_extension_options. auto_explain explains a plan after
 * it has run, when nothing was counted, so the option is refused there.
 */
static bool
pmu_option_guc_check(const char *option_name, const char *option_value,
					 NodeTag option_type)
{
	GUC_check_errmsg("EXPLAIN option \"%s\" cannot be used by auto_explain",
					 option_name);
	GUC_check_errdetail("auto_explain explains a plan after it has run, "
						"when nothing was counted.");
	return false;
}

/*
 * Check the options once all of them are read: PMU counts a run, so it
 * needs ANALYZE.
 */
static void
pmu_validate_options(ExplainState *es, List *options, ParseState *pstate)
{
	PmuExplainState *state;

	if (prev_validate_options_hook)
		prev_validate_options_hook(es, options, pstate);

	state = GetExplainExtensionState(es, pmu_extension_id);
	if (state == NULL || !state->pmu)
		return;

	if (!es->analyze)
		ereport(ERROR,
				(errcode(ERRCODE_INVALID_PARAMETER_VALUE),
				 errmsg("EXPLAIN option %s requires ANALYZE", "PMU")));

	/*
	 * Open the counters now, so that a system where this backend cannot
	 * count fails the statement before it runs.
	 */
	pmu_open_counters();
}

/*
 * Open the counters of this process, once. The crate says how many file
 * descriptors it needs; they come from PostgreSQL's budget of external
 * files, and go back to it when the opening fails. The crate owns the
 * descriptors themselves and closes them on a failure.
 */
static void
pmu_open_counters(void)
{
	uint32		descriptors = 0;
	int32		os_error = 0;
	int32		status;

	if (pmu_counters_opened)
		return;

	status = pmu_counters_plan(PMU_SYSFS, &descriptors, &os_error);
	if (status != PMU_OK)
		pmu_report(status, os_error);

	for (uint32 taken = 0; taken < descriptors; taken++)
	{
		if (!AcquireExternalFD())
		{
			pmu_release_descriptors(taken);
			pmu_report(PMU_NO_DESCRIPTOR, EMFILE);
		}
	}

	status = pmu_counters_open(&os_error);
	if (status != PMU_OK)
	{
		pmu_release_descriptors(descriptors);
		pmu_report(status, os_error);
	}
	pmu_counters_opened = true;
}

static void
pmu_release_descriptors(uint32 n)
{
	for (uint32 i = 0; i < n; i++)
		ReleaseExternalFD();
}

/*
 * Turn a code of the crate into an error of PostgreSQL.
 */
static void
pmu_report(int32 status, int32 os_error)
{
	errno = os_error;

	switch (status)
	{
		case PMU_NOT_PERMITTED:
			ereport(ERROR,
					(errcode(ERRCODE_INSUFFICIENT_PRIVILEGE),
					 errmsg("the system does not allow this process to count instructions: %m"),
					 errhint("On Linux, set kernel.perf_event_paranoid to 2 or less, "
							 "or give the server's processes CAP_PERFMON; "
							 "in a container, its filter of system calls may refuse "
							 "perf_event_open too.")));
			break;
		case PMU_NO_SYSTEM:
			ereport(ERROR,
					(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
					 errmsg("pg_pmu cannot count on this system"),
					 errdetail("It counts on Linux and macOS, and this build of it "
							   "has no counters.")));
			break;
		case PMU_NO_COUNTERS:
			ereport(ERROR,
					(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
					 errmsg("the system keeps no counters of instructions for this process"),
					 errdetail("A virtual machine often gives none.")));
			break;
		case PMU_CORES_NOT_COVERED:
			ereport(ERROR,
					(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
					 errmsg("the kinds of cores in sysfs do not cover all the processors online"),
					 errdetail("Counting on some cores only would show a part of the work.")));
			break;
		case PMU_KIND_NOT_OPENABLE:
			ereport(ERROR,
					(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
					 errmsg("the kernel cannot count on every kind of cores of this processor: %m"),
					 errhint("On Intel this needs Linux 5.13 or later.")));
			break;
		case PMU_NO_FREE_COUNTER:
			ereport(ERROR,
					(errcode(ERRCODE_INSUFFICIENT_RESOURCES),
					 errmsg("no hardware counter is free for this process")));
			break;
		case PMU_NO_DESCRIPTOR:
			ereport(ERROR,
					(errcode(ERRCODE_INSUFFICIENT_RESOURCES),
					 errmsg("no file descriptor is left for the counters: %m")));
			break;
		case PMU_COUNTER_LOST:
			ereport(ERROR,
					(errcode(ERRCODE_INSUFFICIENT_RESOURCES),
					 errmsg("the system took a counter away while the statement ran")));
			break;
		case PMU_SYSTEM_ERROR:
			ereport(ERROR,
					(errcode(ERRCODE_INTERNAL_ERROR),
					 errmsg("the counters of the processor failed: %m")));
			break;
		default:
			ereport(ERROR,
					(errcode(ERRCODE_INTERNAL_ERROR),
					 errmsg("internal error of pg_pmu: code %d", status)));
			break;
	}
	pg_unreachable();
}
