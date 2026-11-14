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
#include "commands/explain.h"
#include "commands/explain_format.h"
#include "commands/explain_state.h"
#include "executor/executor.h"
#include "fmgr.h"
#include "miscadmin.h"
#include "optimizer/planner.h"
#include "storage/fd.h"
#include "tcop/utility.h"
#include "utils/guc.h"

#include "pg_pmu_core.h"

PG_MODULE_MAGIC_EXT(.name = "pg_pmu",.version = "0.1.0");

/* The root of sysfs, where Linux lists the kinds of cores. */
#define PMU_SYSFS "/sys"

/* In test/abi.c. */
extern void pmu_check_layout(void);
extern void pmu_check_perf_abi(void);

/*
 * One start of the executor at the depth of a counting EXPLAIN: a
 * candidate for the statement. Its counts are read when the executor
 * starts and when it has finished; the end of the executor removes it.
 */
typedef struct PmuRecord
{
	struct PmuRecord *next;
	QueryDesc  *query;			/* until its end */
	PlannedStmt *plan;
	PmuCounts	start;
	PmuCounts	finish;
	bool		finished;
} PmuRecord;

/*
 * One EXPLAIN, from the start of its utility statement to its return or
 * failure. Frames make a stack: an EXPLAIN that a function runs inside
 * another one gets a frame above it.
 */
typedef struct PmuFrame
{
	struct PmuFrame *prev;
	int			depth;			/* the depth of its EXPLAIN */
	bool		counting;		/* the option PMU is on */
	bool		referenced;		/* it holds a reference to the counters */
	MemoryContext cxt;			/* where its records live */
	PmuRecord  *records;		/* the newest first */
} PmuFrame;

/*
 * What the module keeps in the state of one EXPLAIN. It lives in the
 * memory of that EXPLAIN and dies with it.
 */
typedef struct PmuExplainState
{
	bool		pmu;			/* the option PMU is on */
	PmuFrame   *frame;			/* its frame, once it counts */
} PmuExplainState;

/* The top of the stack of frames. */
static PmuFrame *pmu_frames = NULL;

/*
 * One more inside the planner and inside each start, run and finish of
 * the executor; the statement of an EXPLAIN starts at the depth of its
 * frame, the queries it runs inside deeper.
 */
static int	pmu_depth = 0;

/* pg_pmu.max_nested: nested counting statements the server runs at once */
static int	pmu_max_nested = 16;

/* Our slot in the extension state of an EXPLAIN. */
static int	pmu_extension_id;

/* The counters of this process are open; they stay so until it exits. */
static bool pmu_counters_opened = false;

static explain_validate_options_hook_type prev_validate_options_hook = NULL;
static explain_per_plan_hook_type prev_explain_per_plan_hook = NULL;
static ProcessUtility_hook_type prev_ProcessUtility = NULL;
static planner_hook_type prev_planner = NULL;
static ExecutorStart_hook_type prev_ExecutorStart = NULL;
static ExecutorRun_hook_type prev_ExecutorRun = NULL;
static ExecutorFinish_hook_type prev_ExecutorFinish = NULL;
static ExecutorEnd_hook_type prev_ExecutorEnd = NULL;

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
static void pmu_read(PmuCounts *now);
static void pmu_frame_pop(PmuFrame *frame, bool report);
static PmuRecord *pmu_find_record(QueryDesc *query);
static void pmu_print(ExplainState *es, const char *phase,
					  const PmuCounts *start, const PmuCounts *end);
static void pmu_explain_per_plan(PlannedStmt *plannedstmt, IntoClause *into,
								 ExplainState *es, const char *queryString,
								 ParamListInfo params,
								 QueryEnvironment *queryEnv);
static void pmu_ProcessUtility(PlannedStmt *pstmt, const char *queryString,
							   bool readOnlyTree,
							   ProcessUtilityContext context,
							   ParamListInfo params,
							   QueryEnvironment *queryEnv,
							   DestReceiver *dest, QueryCompletion *qc);
static PlannedStmt *pmu_planner(Query *parse, const char *query_string,
								int cursorOptions, ParamListInfo boundParams,
								ExplainState *es);
static void pmu_ExecutorStart(QueryDesc *queryDesc, int eflags);
static void pmu_ExecutorRun(QueryDesc *queryDesc, ScanDirection direction,
							uint64 count);
static void pmu_ExecutorFinish(QueryDesc *queryDesc);
static void pmu_ExecutorEnd(QueryDesc *queryDesc);

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

	/*
	 * The header written by hand must match the crate, and on Linux the
	 * crate must match the kernel's header.
	 */
	pmu_check_layout();
	pmu_check_perf_abi();

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
	prev_explain_per_plan_hook = explain_per_plan_hook;
	explain_per_plan_hook = pmu_explain_per_plan;
	prev_ProcessUtility = ProcessUtility_hook;
	ProcessUtility_hook = pmu_ProcessUtility;
	prev_planner = planner_hook;
	planner_hook = pmu_planner;
	prev_ExecutorStart = ExecutorStart_hook;
	ExecutorStart_hook = pmu_ExecutorStart;
	prev_ExecutorRun = ExecutorRun_hook;
	ExecutorRun_hook = pmu_ExecutorRun;
	prev_ExecutorFinish = ExecutorFinish_hook;
	ExecutorFinish_hook = pmu_ExecutorFinish;
	prev_ExecutorEnd = ExecutorEnd_hook;
	ExecutorEnd_hook = pmu_ExecutorEnd;
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
	 * The top frame is this EXPLAIN's, made by the hook of utility
	 * statements at this depth; an EXPLAIN run in any other way cannot
	 * count.
	 */
	if (pmu_frames == NULL || pmu_frames->depth != pmu_depth ||
		pmu_frames->counting)
		ereport(ERROR,
				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
				 errmsg("EXPLAIN option %s works only in an EXPLAIN statement",
						"PMU")));

	/*
	 * Open the counters now, so that a system where this backend cannot
	 * count fails the statement before it runs, and take the frame's
	 * reference, which keeps them counting until the frame ends.
	 */
	pmu_open_counters();
	{
		PmuCounts	now;
		int32		os_error = 0;
		int32		status = pmu_counters_acquire(&now, &os_error);

		if (status != PMU_OK)
			pmu_report(status, os_error);
	}
	pmu_frames->referenced = true;
	pmu_frames->counting = true;
	state->frame = pmu_frames;
}

/*
 * Read the counters, which a reference holds counting.
 */
static void
pmu_read(PmuCounts *now)
{
	int32		os_error = 0;
	int32		status = pmu_counters_read(now, &os_error);

	if (status != PMU_OK)
		pmu_report(status, os_error);
}

/*
 * The hook of utility statements: a frame for each EXPLAIN, taken off
 * the stack when it returns or fails.
 */
static void
pmu_ProcessUtility(PlannedStmt *pstmt, const char *queryString,
				   bool readOnlyTree, ProcessUtilityContext context,
				   ParamListInfo params, QueryEnvironment *queryEnv,
				   DestReceiver *dest, QueryCompletion *qc)
{
	PmuFrame   *frame = NULL;

	if (IsA(pstmt->utilityStmt, ExplainStmt))
	{
		frame = palloc0_object(PmuFrame);
		frame->prev = pmu_frames;
		frame->depth = pmu_depth;
		frame->cxt = CurrentMemoryContext;
		pmu_frames = frame;
	}

	PG_TRY();
	{
		if (prev_ProcessUtility)
			prev_ProcessUtility(pstmt, queryString, readOnlyTree, context,
								params, queryEnv, dest, qc);
		else
			standard_ProcessUtility(pstmt, queryString, readOnlyTree,
									context, params, queryEnv, dest, qc);
	}
	PG_CATCH();
	{
		if (frame != NULL)
			pmu_frame_pop(frame, false);
		PG_RE_THROW();
	}
	PG_END_TRY();

	if (frame != NULL)
		pmu_frame_pop(frame, true);
}

/*
 * Take a frame off the stack and give back its reference. On the way
 * out of an error nothing more is raised.
 */
static void
pmu_frame_pop(PmuFrame *frame, bool report)
{
	Assert(pmu_frames == frame);
	pmu_frames = frame->prev;
	frame->records = NULL;

	if (frame->referenced)
	{
		int32		os_error = 0;
		int32		status = pmu_counters_release(&os_error);

		frame->referenced = false;
		if (status != PMU_OK && report)
			pmu_report(status, os_error);
	}
}

/*
 * The record of an execution that has not ended, in any frame.
 */
static PmuRecord *
pmu_find_record(QueryDesc *query)
{
	for (PmuFrame *f = pmu_frames; f != NULL; f = f->prev)
		for (PmuRecord *r = f->records; r != NULL; r = r->next)
			if (r->query == query)
				return r;
	return NULL;
}

static PlannedStmt *
pmu_planner(Query *parse, const char *query_string, int cursorOptions,
			ParamListInfo boundParams, ExplainState *es)
{
	PlannedStmt *result;

	pmu_depth++;
	PG_TRY();
	{
		if (prev_planner)
			result = prev_planner(parse, query_string, cursorOptions,
								  boundParams, es);
		else
			result = standard_planner(parse, query_string, cursorOptions,
									  boundParams, es);
	}
	PG_FINALLY();
	{
		pmu_depth--;
	}
	PG_END_TRY();

	return result;
}

/*
 * A start of the executor at the depth of a counting frame is a
 * candidate: read the counters and keep a record of it.
 */
static void
pmu_ExecutorStart(QueryDesc *queryDesc, int eflags)
{
	PmuFrame   *frame = pmu_frames;

	if (frame != NULL && frame->counting && pmu_depth == frame->depth)
	{
		PmuRecord  *record = MemoryContextAllocZero(frame->cxt,
													sizeof(PmuRecord));

		record->query = queryDesc;
		record->plan = queryDesc->plannedstmt;
		pmu_read(&record->start);
		record->next = frame->records;
		frame->records = record;
	}

	pmu_depth++;
	PG_TRY();
	{
		if (prev_ExecutorStart)
			prev_ExecutorStart(queryDesc, eflags);
		else
			standard_ExecutorStart(queryDesc, eflags);
	}
	PG_FINALLY();
	{
		pmu_depth--;
	}
	PG_END_TRY();
}

static void
pmu_ExecutorRun(QueryDesc *queryDesc, ScanDirection direction, uint64 count)
{
	pmu_depth++;
	PG_TRY();
	{
		if (prev_ExecutorRun)
			prev_ExecutorRun(queryDesc, direction, count);
		else
			standard_ExecutorRun(queryDesc, direction, count);
	}
	PG_FINALLY();
	{
		pmu_depth--;
	}
	PG_END_TRY();
}

/*
 * The finish of a candidate, AFTER triggers included, completes its
 * record.
 */
static void
pmu_ExecutorFinish(QueryDesc *queryDesc)
{
	PmuRecord  *record;

	pmu_depth++;
	PG_TRY();
	{
		if (prev_ExecutorFinish)
			prev_ExecutorFinish(queryDesc);
		else
			standard_ExecutorFinish(queryDesc);
	}
	PG_FINALLY();
	{
		pmu_depth--;
	}
	PG_END_TRY();

	record = pmu_find_record(queryDesc);
	if (record != NULL && !record->finished)
	{
		pmu_read(&record->finish);
		record->finished = true;
	}
}

/*
 * The end of an execution removes its record: what runs while a plan is
 * printed has ended by the time the printing hook looks, and so has
 * what computed the parameters of EXPLAIN EXECUTE.
 */
static void
pmu_ExecutorEnd(QueryDesc *queryDesc)
{
	for (PmuFrame *f = pmu_frames; f != NULL; f = f->prev)
		for (PmuRecord **r = &f->records; *r != NULL; r = &(*r)->next)
			if ((*r)->query == queryDesc)
			{
				*r = (*r)->next;
				break;
			}

	if (prev_ExecutorEnd)
		prev_ExecutorEnd(queryDesc);
	else
		standard_ExecutorEnd(queryDesc);
}

/*
 * Print two counts: in text one line, in the other formats integer
 * properties named "<phase> Instructions" and "<phase> Cycles".
 */
static void
pmu_print(ExplainState *es, const char *phase, const PmuCounts *start,
		  const PmuCounts *end)
{
	uint64		instructions = end->instructions - start->instructions;
	uint64		cycles = end->cycles - start->cycles;

	if (es->format == EXPLAIN_FORMAT_TEXT)
	{
		ExplainIndentText(es);
		appendStringInfo(es->str,
						 "%s PMU: instructions=" UINT64_FORMAT
						 " cycles=" UINT64_FORMAT "\n",
						 phase, instructions, cycles);
	}
	else
	{
		char		label[64];

		snprintf(label, sizeof(label), "%s Instructions", phase);
		ExplainPropertyUInteger(label, NULL, instructions, es);
		snprintf(label, sizeof(label), "%s Cycles", phase);
		ExplainPropertyUInteger(label, NULL, cycles, es);
	}
}

/*
 * The hook that runs for each plan after EXPLAIN ANALYZE has run and
 * printed it, before the executor ends: print the counts of the record of
 * this plan whose execution has not ended, then forget the frame's
 * records. It does nothing for an EXPLAIN without the option, and for the
 * plans auto_explain logs.
 */
static void
pmu_explain_per_plan(PlannedStmt *plannedstmt, IntoClause *into,
					 ExplainState *es, const char *queryString,
					 ParamListInfo params, QueryEnvironment *queryEnv)
{
	PmuExplainState *state;
	PmuRecord  *record = NULL;

	if (prev_explain_per_plan_hook)
		prev_explain_per_plan_hook(plannedstmt, into, es, queryString,
								   params, queryEnv);

	state = GetExplainExtensionState(es, pmu_extension_id);
	if (state == NULL || !state->pmu)
		return;

	if (state->frame != NULL)
		for (PmuRecord *r = state->frame->records; r != NULL; r = r->next)
			if (r->plan == plannedstmt && r->finished)
			{
				record = r;
				break;
			}
	if (record == NULL)
		ereport(ERROR,
				(errcode(ERRCODE_INTERNAL_ERROR),
				 errmsg("pg_pmu found no counts of the plan it prints")));

	pmu_print(es, "Execution", &record->start, &record->finish);
	state->frame->records = NULL;
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

/* The error of the system, where the crate gave one. */
#define PMU_SYSTEM_DETAIL() \
	(os_error != 0 ? errdetail("The system said: %m.") : 0)

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
					 errmsg("the system does not allow this process to count instructions"),
					 PMU_SYSTEM_DETAIL(),
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
					 errmsg("the kernel cannot count on every kind of cores of this processor"),
					 PMU_SYSTEM_DETAIL(),
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
					 errmsg("no file descriptor is left for the counters"),
					 PMU_SYSTEM_DETAIL()));
			break;
		case PMU_COUNTER_LOST:
			ereport(ERROR,
					(errcode(ERRCODE_INSUFFICIENT_RESOURCES),
					 errmsg("the system took a counter away while the statement ran")));
			break;
		case PMU_SYSTEM_ERROR:
			ereport(ERROR,
					(errcode(ERRCODE_INTERNAL_ERROR),
					 errmsg("the counters of the processor failed"),
					 PMU_SYSTEM_DETAIL()));
			break;
		default:
			ereport(ERROR,
					(errcode(ERRCODE_INTERNAL_ERROR),
					 errmsg("internal error of pg_pmu: code %d", status)));
			break;
	}
	pg_unreachable();
}
