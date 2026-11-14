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

#include "pg_pmu.h"
#include "pg_pmu_core.h"

PG_MODULE_MAGIC_EXT(.name = "pg_pmu",.version = "0.1.0");

/* The root of sysfs, where Linux lists the kinds of cores. */
#define PMU_SYSFS "/sys"

/* In test/abi.c. */
extern void pmu_check_layout(void);
extern void pmu_check_perf_abi(void);

/*
 * One start of the executor at the depth of a counting EXPLAIN: a
 * candidate for the statement. Its counts, and the cell of the frame's
 * workers, are read when the executor starts and when it has finished;
 * the end of the executor removes it.
 */
typedef struct PmuRecord
{
	struct PmuRecord *next;
	QueryDesc  *query;			/* until its end */
	PlannedStmt *plan;
	PmuHold    *hold;			/* the cell of the frame's workers, if any */
	PmuCounts	start;			/* of this process */
	PmuCounts	finish;
	PmuCellContents cell_start; /* of the workers */
	PmuCellContents cell_finish;
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
	PmuHold    *hold;			/* the cell of its workers, if any */
	int			guc_level;		/* where its setting pg_pmu.cell is saved */
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
	PlannedStmt *planned;		/* the plan the planner made last, if any */
	PmuCounts	plan_start;		/* the counts around that planning */
	PmuCounts	plan_end;
} PmuExplainState;

/* The top of the stack of frames. */
static PmuFrame *pmu_frames = NULL;

/*
 * One more inside the planner and inside each start, run and finish of
 * the executor; the statement of an EXPLAIN starts at the depth of its
 * frame, the queries it runs inside deeper.
 */
static int	pmu_depth = 0;

/*
 * Make a call of a hook one level deeper, and come back to the depth
 * after an error too. Only depths inside a frame are compared, so
 * without a frame the call goes as it is, at no cost.
 */
#define PMU_DEEPER(call) \
	do { \
		if (pmu_frames == NULL) \
			call; \
		else \
		{ \
			pmu_depth++; \
			PG_TRY(); \
			{ \
				call; \
			} \
			PG_FINALLY(); \
			{ \
				pmu_depth--; \
			} \
			PG_END_TRY(); \
		} \
	} while (0)

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

static void pmu_release_descriptors(uint32 n);
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
static void pmu_free_records(PmuFrame *frame);
static PmuRecord *pmu_find_record(QueryDesc *query);
static void pmu_print(ExplainState *es, const char *phase,
					  uint64 instructions, uint64 cycles);
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
	pmu_workers_init();

	/*
	 * The header written by hand must match the crate, and on Linux the
	 * crate must match the kernel's header.
	 */
	pmu_check_layout();
	pmu_check_perf_abi();

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

	/* The cell of its workers; none left in the pool fails it here. */
	pmu_frames->hold = pmu_take_cell(&pmu_frames->guc_level);
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
 * Take a frame off the stack and give back its reference and, at the
 * normal end, its cell. On the way out of an error nothing more is
 * raised, and the cell goes back in the abort that follows, when
 * PostgreSQL has waited for the workers.
 */
static void
pmu_frame_pop(PmuFrame *frame, bool report)
{
	bool		referenced = frame->referenced;
	PmuHold    *hold = frame->hold;
	int			guc_level = frame->guc_level;
	int32		os_error = 0;
	int32		status = PMU_OK;

	Assert(pmu_frames == frame);
	pmu_frames = frame->prev;
	pmu_free_records(frame);
	pfree(frame);

	if (referenced)
		status = pmu_counters_release(&os_error);
	if (!report)
		return;
	if (hold != NULL)
		pmu_return_cell(hold, guc_level);
	if (status != PMU_OK)
		pmu_report(status, os_error);
}

static void
pmu_free_records(PmuFrame *frame)
{
	while (frame->records != NULL)
	{
		PmuRecord  *r = frame->records;

		frame->records = r->next;
		pfree(r);
	}
}

/*
 * The record of an execution that has not ended, in a frame at the
 * present depth. A query dropped without its end leaves its record
 * behind, and a later query at another depth may get its address.
 */
static PmuRecord *
pmu_find_record(QueryDesc *query)
{
	for (PmuFrame *f = pmu_frames; f != NULL; f = f->prev)
		if (f->depth == pmu_depth)
			for (PmuRecord *r = f->records; r != NULL; r = r->next)
				if (r->query == query)
					return r;
	return NULL;
}

/*
 * The hook of the planner sees the state of the EXPLAIN that asked for
 * the plan. When it counts, read the counters around the planner and
 * keep the plan it made; the cache of prepared statements passes no
 * state, so its planning is not counted.
 */
static PlannedStmt *
pmu_planner(Query *parse, const char *query_string, int cursorOptions,
			ParamListInfo boundParams, ExplainState *es)
{
	PlannedStmt *result;
	PmuExplainState *state = NULL;
	PmuCounts	start = {0};

	if (es != NULL)
	{
		state = GetExplainExtensionState(es, pmu_extension_id);
		if (state != NULL && (!state->pmu || state->frame == NULL))
			state = NULL;
	}
	if (state != NULL)
		pmu_read(&start);

	PMU_DEEPER(result = (prev_planner ? prev_planner : standard_planner)
			   (parse, query_string, cursorOptions, boundParams, es));

	if (state != NULL)
	{
		pmu_read(&state->plan_end);
		state->plan_start = start;
		state->planned = result;
	}
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

	pmu_worker_start(queryDesc);

	if (frame != NULL && frame->counting && pmu_depth == frame->depth)
	{
		PmuRecord  *record = MemoryContextAllocZero(frame->cxt,
													sizeof(PmuRecord));

		record->query = queryDesc;
		record->plan = queryDesc->plannedstmt;
		record->hold = frame->hold;
		pmu_hold_read(record->hold, &record->cell_start);
		pmu_read(&record->start);
		record->next = frame->records;
		frame->records = record;
	}

	PMU_DEEPER((prev_ExecutorStart ? prev_ExecutorStart :
				standard_ExecutorStart) (queryDesc, eflags));
}

static void
pmu_ExecutorRun(QueryDesc *queryDesc, ScanDirection direction, uint64 count)
{
	PMU_DEEPER((prev_ExecutorRun ? prev_ExecutorRun :
				standard_ExecutorRun) (queryDesc, direction, count));
}

/*
 * The finish of a candidate, AFTER triggers included, completes its
 * record.
 */
static void
pmu_ExecutorFinish(QueryDesc *queryDesc)
{
	PmuRecord  *record;

	PMU_DEEPER((prev_ExecutorFinish ? prev_ExecutorFinish :
				standard_ExecutorFinish) (queryDesc));

	record = pmu_find_record(queryDesc);
	if (record != NULL && !record->finished)
	{
		pmu_read(&record->finish);
		pmu_hold_read(record->hold, &record->cell_finish);
		record->finished = true;

		/*
		 * A worker that could not count raised its error; when a function
		 * caught it, the counts would leave the worker out.
		 */
		if (record->cell_finish.failures != record->cell_start.failures)
			ereport(ERROR,
					(errcode(record->cell_finish.sqlstate != 0 ?
							 (int) record->cell_finish.sqlstate :
							 ERRCODE_INTERNAL_ERROR),
					 errmsg("a parallel worker of the statement could not count"),
					 errdetail("A function caught the error of the worker.")));
	}
	pmu_worker_finish(queryDesc);
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
				PmuRecord  *ended = *r;

				*r = ended->next;
				pfree(ended);
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
pmu_print(ExplainState *es, const char *phase, uint64 instructions,
		  uint64 cycles)
{
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
 * printed it, before the executor ends: print the counts of its planning,
 * if this EXPLAIN planned it, and those of the record of this plan whose
 * execution has not ended, with the part of the workers under VERBOSE
 * when a worker counted, then forget the frame's records. It does
 * nothing for an EXPLAIN without the option, and for the plans
 * auto_explain logs.
 */
static void
pmu_explain_per_plan(PlannedStmt *plannedstmt, IntoClause *into,
					 ExplainState *es, const char *queryString,
					 ParamListInfo params, QueryEnvironment *queryEnv)
{
	PmuExplainState *state;
	PmuRecord  *record = NULL;
	uint64		workers_instructions;
	uint64		workers_cycles;

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

	if (state->planned == plannedstmt)
		pmu_print(es, "Planning",
				  state->plan_end.instructions - state->plan_start.instructions,
				  state->plan_end.cycles - state->plan_start.cycles);
	state->planned = NULL;

	/* The counts of the execution include the part of the workers. */
	workers_instructions = record->cell_finish.instructions -
		record->cell_start.instructions;
	workers_cycles = record->cell_finish.cycles - record->cell_start.cycles;
	pmu_print(es, "Execution",
			  record->finish.instructions - record->start.instructions +
			  workers_instructions,
			  record->finish.cycles - record->start.cycles + workers_cycles);
	if (es->verbose && workers_instructions > 0)
		pmu_print(es, "Workers", workers_instructions, workers_cycles);
	pmu_free_records(state->frame);
}

/*
 * Open the counters of this process, once. The crate says how many file
 * descriptors it needs; they come from PostgreSQL's budget of external
 * files, and go back to it when the opening fails. The crate owns the
 * descriptors themselves and closes them on a failure.
 */
void
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
 * The SQLSTATE of a code of the crate, as the spec gives it.
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
		case PMU_KIND_NOT_OPENABLE:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("the kernel cannot count on every kind of cores of this processor"),
					 PMU_SYSTEM_DETAIL(),
					 errhint("On Intel this needs Linux 5.13 or later.")));
			break;
		case PMU_NO_FREE_COUNTER:
			ereport(ERROR,
					(errcode(pmu_status_sqlstate(status)),
					 errmsg("no hardware counter is free for this process")));
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
