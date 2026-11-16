/*-------------------------------------------------------------------------
 *
 * pg_pmu.c
 *		Counts of instructions and cycles of the processor in EXPLAIN
 *		ANALYZE, on Greengage.
 *
 * The coordinator (QD) counts its own part of a statement as the module
 * does on PostgreSQL. Each executor of a slice on a segment (QE) counts
 * its part of every EXPLAIN ANALYZE, from the start of its executor to
 * the start of its end, and sends the counts to the QD with the
 * statistics of its slice; the QD adds them in. A QE never raises an
 * error of its own, since it does not know whether the statement asked
 * for PMU: it sends why it could not count, and the QD fails the
 * statement that asked.
 *
 * It needs Greengage with the hooks of EXPLAIN for extensions
 * (commands/explain_state.h, explain_per_plan_hook, cdbexplain_send_hook
 * and cdbexplain_recv_hook), and must be loaded in every backend of the
 * cluster, the QEs' included.
 *
 * Copyright (c) 2026, Denis Smirnov
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#include "access/xact.h"
#include "cdb/cdbexplain.h"
#include "cdb/cdbvars.h"
#include "commands/defrem.h"
#include "commands/explain.h"
#include "commands/explain_state.h"
#include "executor/executor.h"
#include "executor/instrument.h"
#include "fmgr.h"
#include "lib/stringinfo.h"
#include "miscadmin.h"
#include "optimizer/planner.h"
#include "tcop/tcopprot.h"
#include "tcop/utility.h"
#include "utils/memutils.h"

#include "pmu_counters.h"
#include "report.h"

PG_MODULE_MAGIC;

void		_PG_init(void);

/* The root of sysfs, where Linux lists the kinds of cores. */
#define PMU_SYSFS "/sys"

/* The name of the QEs' data in the statistics of a slice. */
#define PMU_PIECE "pg_pmu"

/* In test/abi.c. */
extern void pmu_check_layout(void);
extern void pmu_check_perf_abi(void);

/*
 * What a QE sends: its counts of the statement, or the code of the crate
 * and the error of the system that kept it from counting.
 */
typedef struct PmuQeReport
{
	int32		status;
	int32		os_error;
	uint64		instructions;
	uint64		cycles;
} PmuQeReport;

/*
 * The QEs' reports that one statement took in, or the statements nested
 * in a frame: every message of statistics should carry one.
 */
typedef struct PmuQeSums
{
	uint64		instructions;
	uint64		cycles;
	int			messages;		/* the QEs' messages of statistics */
	int			reports;		/* of them, those with a report of pg_pmu */
	int			failures;		/* reports of QEs that could not count */
	PmuQeReport failure;		/* the last of them */
	int			failure_segment;
} PmuQeSums;

/* The reports of the QEs of one slice, added up on the QD. */
typedef struct PmuSlice
{
	int			slice;
	int			messages;		/* the QEs' messages of statistics */
	int			reports;		/* of them, those with a report */
	int			qes;			/* the QEs that counted */
	uint64		instructions;
	uint64		cycles;
	uint64		max_instructions;	/* of one QE */
	int			max_segment;	/* where it ran */
} PmuSlice;

/*
 * One start of the executor at the depth of a counting EXPLAIN: a
 * candidate for the statement. Its counts are read when the executor
 * starts and when it has finished; the end of the executor removes it.
 * The reports of its QEs come with its showstatctx.
 */
typedef struct PmuRecord
{
	struct PmuRecord *next;
	QueryDesc  *query;			/* until its end */
	PlannedStmt *plan;
	struct CdbExplain_ShowStatCtx *showstatctx;
	struct PmuFrame *frame;		/* where it lives */
	PmuCounts	start;			/* of the QD */
	PmuCounts	finish;
	PmuQeSums	nested_start;	/* the frame's nested sums at its start */
	PmuQeSums	nested_finish;	/* and at its finish */
	bool		finished;
	PmuQeSums	sums;			/* of all its QEs */
	PmuSlice   *slices;			/* in the frame's memory */
	int			nslices;
	int			allocated;
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
	bool		counting;		/* the option PMU is on; it then holds a
								 * reference to the counters */
	MemoryContext cxt;			/* where its records live */
	PmuRecord  *records;		/* the newest first */
	PmuQeSums	nested;			/* the QEs of the statements nested in it */
	PmuQeSums	handed;			/* the QEs of its printed records */
	const char *lost;			/* why its counts would be short, if they
								 * would */
} PmuFrame;

/*
 * What the module keeps in the state of one EXPLAIN. It lives in the
 * memory of that EXPLAIN and dies with it.
 */
typedef struct PmuExplainState
{
	bool		pmu;			/* the option PMU is on */
	PmuFrame   *frame;			/* its frame, once it counts */
	PlannedStmt *planned;		/* the plan this EXPLAIN made, if any */
	PmuCounts	plan_start;		/* the counts around that planning */
	PmuCounts	plan_end;
	PmuQeSums	plan_nested;	/* the QEs of statements nested in it */
} PmuExplainState;

/*
 * A statement a QE counts. It lives in TopMemoryContext: the end of the
 * executor frees it, the abort of its subtransaction or the end of the
 * transaction those an error left behind.
 */
typedef struct PmuQeRun
{
	struct PmuQeRun *next;
	QueryDesc  *query;
	SubTransactionId subxid;	/* where it started */
	bool		counting;		/* it holds a reference to the counters */
	PmuCounts	start;
	PmuQeReport report;
} PmuQeRun;

/* The top of the stack of frames, on the QD. */
static PmuFrame *pmu_frames = NULL;

/*
 * One more inside the planner and inside each start, run and finish of
 * the executor; the statement of an EXPLAIN starts at the depth of its
 * frame, the queries it runs inside deeper.
 */
static int	pmu_depth = 0;

/* The statements this QE counts, the newest first. */
static PmuQeRun *pmu_qe_runs = NULL;

/*
 * Make a call of a hook one level deeper, and come back to the depth
 * after an error too. Only depths inside a frame are compared, so
 * without a frame the call goes as it is.
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
			PG_CATCH(); \
			{ \
				pmu_depth--; \
				PG_RE_THROW(); \
			} \
			PG_END_TRY(); \
			pmu_depth--; \
		} \
	} while (0)

/* Our slot in the extension state of an EXPLAIN. */
static int	pmu_extension_id;

/* The counters of this process are open; they stay so until it exits. */
static bool pmu_counters_opened = false;

static explain_validate_options_hook_type prev_validate_options_hook = NULL;
static explain_per_plan_hook_type prev_explain_per_plan_hook = NULL;
static ExplainOneQuery_hook_type prev_ExplainOneQuery = NULL;
static ProcessUtility_hook_type prev_ProcessUtility = NULL;
static planner_hook_type prev_planner = NULL;
static ExecutorStart_hook_type prev_ExecutorStart = NULL;
static ExecutorRun_hook_type prev_ExecutorRun = NULL;
static ExecutorFinish_hook_type prev_ExecutorFinish = NULL;
static ExecutorEnd_hook_type prev_ExecutorEnd = NULL;
static cdbexplain_send_hook_type prev_cdbexplain_send = NULL;
static cdbexplain_recv_hook_type prev_cdbexplain_recv = NULL;

static int32 pmu_try_open(int32 *os_error);
static void pmu_open_counters(void);
static PmuExplainState *pmu_ensure_state(ExplainState *es);
static void pmu_option_handler(ExplainState *es, DefElem *opt,
							   ParseState *pstate);
static void pmu_validate_options(ExplainState *es, List *options,
								 ParseState *pstate);
static void pmu_read(PmuCounts *now);
static void pmu_frame_pop(PmuFrame *frame, bool report);
static void pmu_free_records(PmuFrame *frame);
static PmuRecord *pmu_find_record(QueryDesc *query);
static void pmu_print(ExplainState *es, const char *phase,
					  uint64 instructions, uint64 cycles);
static void pmu_print_slices(ExplainState *es, PmuRecord *record);
static void pmu_explain_per_plan(PlannedStmt *plannedstmt, IntoClause *into,
								 ExplainState *es, const char *queryString,
								 ParamListInfo params,
								 QueryEnvironment *queryEnv);
static void pmu_ExplainOneQuery(Query *query, int cursorOptions,
								IntoClause *into, ExplainState *es,
								const char *queryString, ParamListInfo params,
								QueryEnvironment *queryEnv);
static void pmu_ProcessUtility(PlannedStmt *pstmt, const char *queryString,
							   ProcessUtilityContext context,
							   ParamListInfo params,
							   QueryEnvironment *queryEnv,
							   DestReceiver *dest, char *completionTag);
static PlannedStmt *pmu_planner(Query *parse, int cursorOptions,
								ParamListInfo boundParams);
static void pmu_ExecutorStart(QueryDesc *queryDesc, int eflags);
static void pmu_ExecutorRun(QueryDesc *queryDesc, ScanDirection direction,
							uint64 count, bool execute_once);
static void pmu_ExecutorFinish(QueryDesc *queryDesc);
static void pmu_ExecutorEnd(QueryDesc *queryDesc);
static bool pmu_qe_explained(QueryDesc *queryDesc);
static PmuQeRun *pmu_qe_find(QueryDesc *queryDesc);
static void pmu_qe_start(QueryDesc *queryDesc);
static void pmu_qe_stop(PmuQeRun *run);
static void pmu_qe_forget(PmuQeRun *run);
static void pmu_xact_callback(XactEvent event, void *arg);
static void pmu_subxact_callback(SubXactEvent event, SubTransactionId mySubid,
								 SubTransactionId parentSubid, void *arg);
static PmuFrame *pmu_counting_frame(PmuFrame *from);
static bool pmu_runs_on_segments(QueryDesc *queryDesc);
static bool pmu_explained_inside(QueryDesc *queryDesc);
static void pmu_check_unexplained(QueryDesc *queryDesc, bool started);
static void pmu_sums_add(PmuQeSums *to, const PmuQeSums *from);
static PmuQeSums pmu_sums_since(const PmuQeSums *now, const PmuQeSums *then);
static bool pmu_status_lasts(int32 status);
static PmuSlice *pmu_record_slice(PmuRecord *record, PmuFrame *frame,
								  int sliceIndex);
static void pmu_check_counts(PmuFrame *frame, PmuRecord *record,
							 const PmuQeSums *nested);
static void pmu_fail_report(const PmuQeSums *sums, const char *what);
static void pmu_cdbexplain_send(QueryDesc *queryDesc, StringInfo buf);
static void pmu_cdbexplain_recv(struct CdbExplain_ShowStatCtx *showstatctx,
								int sliceIndex, int segindex,
								int qe_identifier, const char *name,
								const char *data, int len);
static const char *pmu_status_text(int32 status);

void
_PG_init(void)
{
	/*
	 * The header written by hand must match the crate, and on Linux the
	 * crate must match the kernel's header.
	 */
	pmu_check_layout();
	pmu_check_perf_abi();

	pmu_extension_id = GetExplainExtensionId("pg_pmu");
	RegisterExtensionExplainOption("pmu", pmu_option_handler);

	prev_validate_options_hook = explain_validate_options_hook;
	explain_validate_options_hook = pmu_validate_options;
	prev_explain_per_plan_hook = explain_per_plan_hook;
	explain_per_plan_hook = pmu_explain_per_plan;
	prev_ExplainOneQuery = ExplainOneQuery_hook;
	ExplainOneQuery_hook = pmu_ExplainOneQuery;
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
	prev_cdbexplain_send = cdbexplain_send_hook;
	cdbexplain_send_hook = pmu_cdbexplain_send;
	prev_cdbexplain_recv = cdbexplain_recv_hook;
	cdbexplain_recv_hook = pmu_cdbexplain_recv;

	RegisterXactCallback(pmu_xact_callback, NULL);
	RegisterSubXactCallback(pmu_subxact_callback, NULL);
}

/*
 * Open the counters of this process, once, and return the code of the
 * crate. PostgreSQL 12 keeps no budget of external files, so the
 * descriptors of the counters are not taken from one. A QE that the
 * system refused does not try again: it counts every EXPLAIN ANALYZE,
 * and such a refusal lasts; a counter or a descriptor short at the
 * moment is tried again.
 */
static int32 pmu_qe_open_status = PMU_OK;
static int32 pmu_qe_open_error = 0;

static int32
pmu_try_open(int32 *os_error)
{
	uint32		descriptors = 0;
	int32		status;

	if (pmu_counters_opened)
		return PMU_OK;
	if (pmu_qe_open_status != PMU_OK)
	{
		*os_error = pmu_qe_open_error;
		return pmu_qe_open_status;
	}
	status = pmu_counters_plan(PMU_SYSFS, PMU_EVENTS_BASIC, &descriptors,
							   os_error);
	if (status == PMU_OK)
		status = pmu_counters_open(PMU_EVENTS_BASIC, os_error);
	if (status == PMU_OK)
		pmu_counters_opened = true;
	else if (Gp_role == GP_ROLE_EXECUTE && pmu_status_lasts(status))
	{
		pmu_qe_open_status = status;
		pmu_qe_open_error = *os_error;
	}
	return status;
}

/*
 * Whether a code of the crate says the system cannot count at all, not
 * that a counter or a descriptor was short at the moment.
 */
static bool
pmu_status_lasts(int32 status)
{
	switch (status)
	{
		case PMU_NOT_PERMITTED:
		case PMU_NO_SYSTEM:
		case PMU_NO_COUNTERS:
		case PMU_CORES_NOT_COVERED:
		case PMU_KIND_NOT_OPENABLE:
		case PMU_TOO_MANY_KINDS:
			return true;
		default:
			return false;
	}
}

/* Open the counters of the QD, or fail the statement. */
static void
pmu_open_counters(void)
{
	int32		os_error = 0;
	int32		status = pmu_try_open(&os_error);

	if (status != PMU_OK)
		pmu_report(status, os_error);
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
		state = palloc0(sizeof(PmuExplainState));
		SetExplainExtensionState(es, pmu_extension_id, state);
	}
	return state;
}

/*
 * EXPLAIN (PMU [boolean]): takes the values of the core's own boolean
 * options.
 */
static void
pmu_option_handler(ExplainState *es, DefElem *opt, ParseState *pstate)
{
	pmu_ensure_state(es)->pmu = defGetBoolean(opt);
}

/*
 * Check the options once all of them are read: PMU counts a run, so it
 * needs ANALYZE. Then open the counters of the QD and take the frame's
 * reference, which keeps them counting until the frame ends.
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

	/* With ORCA, DXL prints the plan and runs nothing. */
	if (es->dxl)
		ereport(ERROR,
				(errcode(ERRCODE_INVALID_PARAMETER_VALUE),
				 errmsg("EXPLAIN options %s and %s cannot be used together",
						"PMU", "DXL")));

	if (Gp_role != GP_ROLE_DISPATCH)
		ereport(ERROR,
				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
				 errmsg("EXPLAIN option %s works only on the coordinator", "PMU")));

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

	pmu_open_counters();
	{
		PmuCounts	now;
		int32		os_error = 0;
		int32		status = pmu_counters_acquire(PMU_EVENTS_BASIC, &now,
												  &os_error);

		if (status != PMU_OK)
			pmu_report(status, os_error);
	}
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
	int32		status = pmu_counters_read(PMU_EVENTS_BASIC, now, &os_error);

	if (status != PMU_OK)
		pmu_report(status, os_error);
}

/*
 * The hook of utility statements: a frame for each EXPLAIN, taken off
 * the stack when it returns or fails.
 */
static void
pmu_ProcessUtility(PlannedStmt *pstmt, const char *queryString,
				   ProcessUtilityContext context, ParamListInfo params,
				   QueryEnvironment *queryEnv, DestReceiver *dest,
				   char *completionTag)
{
	PmuFrame   *frame = NULL;

	if (IsA(pstmt->utilityStmt, ExplainStmt))
	{
		frame = palloc0(sizeof(PmuFrame));
		frame->prev = pmu_frames;
		frame->depth = pmu_depth;
		frame->cxt = CurrentMemoryContext;
		pmu_frames = frame;
	}

	PG_TRY();
	{
		if (prev_ProcessUtility)
			prev_ProcessUtility(pstmt, queryString, context, params,
								queryEnv, dest, completionTag);
		else
			standard_ProcessUtility(pstmt, queryString, context, params,
									queryEnv, dest, completionTag);
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
 * Take a frame off the stack and give back its reference. On the way out
 * of an error nothing more is raised.
 */
static void
pmu_frame_pop(PmuFrame *frame, bool report)
{
	bool		referenced = frame->counting;
	int32		os_error = 0;
	int32		status = PMU_OK;

	PmuFrame   *enclosing;

	Assert(pmu_frames == frame);
	pmu_frames = frame->prev;

	/*
	 * The work of the QEs inside this EXPLAIN is a part of an enclosing
	 * counting statement: its printed records' and its nested ones'. An
	 * EXPLAIN that fails, even one a function catches, loses the counts
	 * its QEs had not sent yet.
	 */
	enclosing = pmu_counting_frame(pmu_frames);
	if (enclosing != NULL)
	{
		if (report)
		{
			pmu_sums_add(&enclosing->nested, &frame->handed);
			pmu_sums_add(&enclosing->nested, &frame->nested);
			if (frame->lost != NULL && enclosing->lost == NULL)
				enclosing->lost = frame->lost;
		}
		else
			enclosing->lost = "an EXPLAIN inside the statement failed, "
				"and the counts of its segments are lost";
	}

	pmu_free_records(frame);
	pfree(frame);

	if (referenced)
		status = pmu_counters_release(PMU_EVENTS_BASIC, &os_error);
	if (report && status != PMU_OK)
		pmu_report(status, os_error);
}

static void
pmu_free_records(PmuFrame *frame)
{
	while (frame->records != NULL)
	{
		PmuRecord  *r = frame->records;

		frame->records = r->next;
		if (r->slices != NULL)
			pfree(r->slices);
		pfree(r);
	}
}

/*
 * The record of an execution that has not ended, in a frame at the
 * present depth.
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
 * Plan the statement of a counting EXPLAIN between two reads of the
 * counters, then explain the plan as the core does. Planning in another
 * extension's hook of this kind, or in the cache of prepared statements,
 * is not counted.
 */
static void
pmu_ExplainOneQuery(Query *query, int cursorOptions, IntoClause *into,
					ExplainState *es, const char *queryString,
					ParamListInfo params, QueryEnvironment *queryEnv)
{
	PmuExplainState *state = GetExplainExtensionState(es, pmu_extension_id);
	PlannedStmt *plan;
	instr_time	planstart,
				planduration;
	PmuCounts	start = {0};
	PmuQeSums	nested_start;
	bool		counting;

	if (prev_ExplainOneQuery)
	{
		prev_ExplainOneQuery(query, cursorOptions, into, es, queryString,
							 params, queryEnv);
		return;
	}

	counting = state != NULL && state->pmu && state->frame != NULL;
	INSTR_TIME_SET_CURRENT(planstart);
	if (counting)
	{
		nested_start = state->frame->nested;
		pmu_read(&start);
	}

	plan = pg_plan_query(query, cursorOptions, params);

	if (counting)
	{
		pmu_read(&state->plan_end);
		state->plan_start = start;
		state->plan_nested = pmu_sums_since(&state->frame->nested,
											&nested_start);
		state->planned = plan;
	}
	INSTR_TIME_SET_CURRENT(planduration);
	INSTR_TIME_SUBTRACT(planduration, planstart);

	ExplainOnePlan(plan, into, es, queryString, params, queryEnv,
				   &planduration, cursorOptions);
}

/* Inside the planner the queries it runs are deeper. */
static PlannedStmt *
pmu_planner(Query *parse, int cursorOptions, ParamListInfo boundParams)
{
	PlannedStmt *result;

	PMU_DEEPER(result = (prev_planner ? prev_planner : standard_planner)
			   (parse, cursorOptions, boundParams));
	return result;
}

/*
 * On the QD, a start of the executor at the depth of a counting frame is
 * a candidate: read the counters and keep a record of it, before the
 * core starts it, since the reports of its initplans come in during the
 * start. On a QE, a slice of an EXPLAIN ANALYZE starts counting.
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
		record->showstatctx = queryDesc->showstatctx;
		record->frame = frame;
		record->nested_start = frame->nested;
		pmu_read(&record->start);
		record->next = frame->records;
		frame->records = record;
	}

	/* Before it starts, as its start may fail and a function catch it. */
	if ((eflags & EXEC_FLAG_EXPLAIN_ONLY) == 0)
		pmu_check_unexplained(queryDesc, false);

	if (pmu_qe_explained(queryDesc))
		pmu_qe_start(queryDesc);

	PMU_DEEPER((prev_ExecutorStart ? prev_ExecutorStart :
				standard_ExecutorStart) (queryDesc, eflags));
}

/* The innermost counting frame from this one down, or NULL. */
static PmuFrame *
pmu_counting_frame(PmuFrame *from)
{
	for (PmuFrame *f = from; f != NULL; f = f->prev)
		if (f->counting)
			return f;
	return NULL;
}

/*
 * Whether a plan sends slices to the segments: a CREATE TABLE AS WITH NO
 * DATA dispatches nothing.
 */
static bool
pmu_runs_on_segments(QueryDesc *queryDesc)
{
	PlannedStmt *plan = queryDesc->plannedstmt;

	if (plan->intoClause != NULL && plan->intoClause->skipData)
		return false;
	for (int i = 0; i < plan->numSlices; i++)
		if (plan->slices[i].gangType != GANGTYPE_UNALLOCATED)
			return true;
	return false;
}

/*
 * Whether a query is the statement of an EXPLAIN ANALYZE inside the
 * counting one, which takes in its QEs' statistics: instrumented, and
 * started at the depth of the top frame. A query that auto_explain
 * instruments is not: auto_explain may never take its statistics in.
 */
static bool
pmu_explained_inside(QueryDesc *queryDesc)
{
	return (queryDesc->instrument_options & INSTRUMENT_CDB) != 0 &&
		pmu_frames != NULL && pmu_frames->depth == pmu_depth;
}

/*
 * A query inside a counting statement that runs on the segments outside
 * an EXPLAIN ANALYZE gets no statistics from its QEs, and so no counts:
 * the statement cannot count its whole work. Checked at each start of
 * the executor, and at each run that moves: a cursor opened before the
 * statement and fetched inside it makes its QEs work there without a
 * start. They may work while its main plan is dispatched, as the QD
 * cannot tell when they are done; its initplans ran at its start. Its
 * close inside only cuts short the work its QEs do on their own since
 * it opened.
 */
static void
pmu_check_unexplained(QueryDesc *queryDesc, bool started)
{
	PmuFrame   *counting = pmu_counting_frame(pmu_frames);

	if (Gp_role == GP_ROLE_DISPATCH && counting != NULL &&
		pmu_depth > counting->depth &&
		!pmu_explained_inside(queryDesc) &&
		(started ? queryDesc->estate->dispatcherState != NULL :
		 pmu_runs_on_segments(queryDesc)))
		counting->lost = "a query inside the statement ran on the segments "
			"outside an EXPLAIN ANALYZE, and only what EXPLAIN ANALYZE runs "
			"is counted there";
}

/* Add the counts and the tallies of one sum to another. */
static void
pmu_sums_add(PmuQeSums *to, const PmuQeSums *from)
{
	to->instructions += from->instructions;
	to->cycles += from->cycles;
	to->messages += from->messages;
	to->reports += from->reports;
	if (from->failures > 0)
	{
		to->failures += from->failures;
		to->failure = from->failure;
		to->failure_segment = from->failure_segment;
	}
}

/* What a sum gained since an earlier copy of it. */
static PmuQeSums
pmu_sums_since(const PmuQeSums *now, const PmuQeSums *then)
{
	PmuQeSums	d = *now;

	d.instructions -= then->instructions;
	d.cycles -= then->cycles;
	d.messages -= then->messages;
	d.reports -= then->reports;
	d.failures -= then->failures;
	return d;
}

static void
pmu_ExecutorRun(QueryDesc *queryDesc, ScanDirection direction, uint64 count,
				bool execute_once)
{
	/* A run without movement, such as a fetch at the end, runs no plan. */
	if (!ScanDirectionIsNoMovement(direction))
		pmu_check_unexplained(queryDesc, true);
	PMU_DEEPER((prev_ExecutorRun ? prev_ExecutorRun :
				standard_ExecutorRun) (queryDesc, direction, count,
									   execute_once));
}

/*
 * The finish of a candidate, AFTER triggers included, completes the QD's
 * part of its record.
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
		record->nested_finish = record->frame->nested;
		record->finished = true;
	}
}

/*
 * On the QD, the end of an execution removes its record. On a QE, the
 * counts of the slice are read before the core ends the executor and
 * sends the statistics with them.
 */
static void
pmu_ExecutorEnd(QueryDesc *queryDesc)
{
	PmuQeRun   *run = pmu_qe_find(queryDesc);

	for (PmuFrame *f = pmu_frames; f != NULL; f = f->prev)
		for (PmuRecord **r = &f->records; *r != NULL; r = &(*r)->next)
			if ((*r)->query == queryDesc)
			{
				PmuRecord  *ended = *r;

				*r = ended->next;
				if (ended->slices != NULL)
					pfree(ended->slices);
				pfree(ended);
				break;
			}

	if (run != NULL)
		pmu_qe_stop(run);

	if (prev_ExecutorEnd)
		prev_ExecutorEnd(queryDesc);
	else
		standard_ExecutorEnd(queryDesc);

	if (run != NULL)
		pmu_qe_forget(run);
}

/* Whether a QE runs a slice of an EXPLAIN ANALYZE. */
static bool
pmu_qe_explained(QueryDesc *queryDesc)
{
	return Gp_role == GP_ROLE_EXECUTE &&
		queryDesc->ddesc != NULL &&
		queryDesc->ddesc->sliceTable != NULL &&
		(queryDesc->ddesc->sliceTable->instrument_options & INSTRUMENT_CDB) != 0;
}

static PmuQeRun *
pmu_qe_find(QueryDesc *queryDesc)
{
	for (PmuQeRun *run = pmu_qe_runs; run != NULL; run = run->next)
		if (run->query == queryDesc)
			return run;
	return NULL;
}

/*
 * Start counting a slice on a QE. What keeps it from counting goes into
 * its report, not into an error.
 */
static void
pmu_qe_start(QueryDesc *queryDesc)
{
	PmuQeRun   *run = MemoryContextAllocZero(TopMemoryContext,
											 sizeof(PmuQeRun));
	int32		os_error = 0;
	int32		status;

	run->query = queryDesc;
	run->subxid = GetCurrentSubTransactionId();
	run->next = pmu_qe_runs;
	pmu_qe_runs = run;

#ifdef PMU_TEST_QES_CANNOT_COUNT
	/* A build for the suite qes_cannot_count: no QE can count. */
	status = PMU_NO_COUNTERS;
#else
	status = pmu_try_open(&os_error);
	if (status == PMU_OK)
		status = pmu_counters_acquire(PMU_EVENTS_BASIC, &run->start,
									  &os_error);
#endif
	run->counting = status == PMU_OK;
	run->report.status = status;
	run->report.os_error = os_error;
}

/* Read the counts of a slice and give back its reference. */
static void
pmu_qe_stop(PmuQeRun *run)
{
	PmuCounts	now;
	int32		os_error = 0;
	int32		status;

	if (!run->counting)
		return;
	status = pmu_counters_read(PMU_EVENTS_BASIC, &now, &os_error);
	if (status == PMU_OK)
	{
		run->report.instructions = now.instructions - run->start.instructions;
		run->report.cycles = now.cycles - run->start.cycles;
	}
	else
	{
		run->report.status = status;
		run->report.os_error = os_error;
	}
	run->counting = false;
	os_error = 0;
	status = pmu_counters_release(PMU_EVENTS_BASIC, &os_error);
	if (status != PMU_OK && run->report.status == PMU_OK)
	{
		run->report.status = status;
		run->report.os_error = os_error;
	}
}

/* Take a run off the list, giving back its reference if it holds one. */
static void
pmu_qe_forget(PmuQeRun *run)
{
	int32		os_error;

	for (PmuQeRun **r = &pmu_qe_runs; *r != NULL; r = &(*r)->next)
		if (*r == run)
		{
			*r = run->next;
			break;
		}
	if (run->counting)
		(void) pmu_counters_release(PMU_EVENTS_BASIC, &os_error);
	pfree(run);
}

/*
 * The runs an error left behind end with the transaction: a run still
 * listed at its end is one whose executor never ended.
 */
static void
pmu_xact_callback(XactEvent event, void *arg)
{
	if (event != XACT_EVENT_ABORT && event != XACT_EVENT_COMMIT &&
		event != XACT_EVENT_PREPARE)
		return;
	while (pmu_qe_runs != NULL)
		pmu_qe_forget(pmu_qe_runs);
}

/*
 * A QE that writes runs inside the QD's subtransactions: the runs that
 * started in one that aborts, or in those nested in it, end with it.
 */
static void
pmu_subxact_callback(SubXactEvent event, SubTransactionId mySubid,
					 SubTransactionId parentSubid, void *arg)
{
	PmuQeRun   *next;

	if (event != SUBXACT_EVENT_ABORT_SUB)
		return;
	for (PmuQeRun *run = pmu_qe_runs; run != NULL; run = next)
	{
		next = run->next;
		if (run->subxid >= mySubid)
			pmu_qe_forget(run);
	}
}

/* On a QE: send the report of the slice with its statistics. */
static void
pmu_cdbexplain_send(QueryDesc *queryDesc, StringInfo buf)
{
	PmuQeRun   *run;

	if (prev_cdbexplain_send)
		prev_cdbexplain_send(queryDesc, buf);

	run = pmu_qe_find(queryDesc);
	if (run != NULL)
		cdbexplain_appendExtensionData(buf, PMU_PIECE, &run->report,
									   sizeof(run->report));
}

/*
 * On the QD: add a QE's message, and then its report, to the record of
 * its statement; a statement nested in a counting one without a record
 * of its own, such as an EXPLAIN ANALYZE without PMU, gives them to the
 * innermost counting frame. Reports when nothing counts are dropped.
 */
static void
pmu_cdbexplain_recv(struct CdbExplain_ShowStatCtx *showstatctx,
					int sliceIndex, int segindex, int qe_identifier,
					const char *name, const char *data, int len)
{
	PmuRecord  *record = NULL;
	PmuFrame   *frame = NULL;
	PmuQeSums  *sums;
	PmuSlice   *slice = NULL;
	PmuQeReport report;

	if (prev_cdbexplain_recv)
		prev_cdbexplain_recv(showstatctx, sliceIndex, segindex,
							 qe_identifier, name, data, len);

	if (name != NULL && strcmp(name, PMU_PIECE) != 0)
		return;
	if (showstatctx != NULL)
		for (PmuFrame *f = pmu_frames; f != NULL && record == NULL; f = f->prev)
			for (PmuRecord *r = f->records; r != NULL; r = r->next)
				if (r->showstatctx == showstatctx)
				{
					record = r;
					frame = f;
					break;
				}
	if (record != NULL)
	{
		sums = &record->sums;
		slice = pmu_record_slice(record, frame, sliceIndex);
	}
	else
	{
		frame = pmu_counting_frame(pmu_frames);
		if (frame == NULL)
			return;
		sums = &frame->nested;
	}

	/* The message comes first, with no name, then its pieces. */
	if (name == NULL)
	{
		sums->messages++;
		if (slice != NULL)
			slice->messages++;
		return;
	}

	if (len != sizeof(report))
		ereport(ERROR,
				(errcode(ERRCODE_INTERNAL_ERROR),
				 errmsg("internal error of pg_pmu: a report of seg%d has %d bytes",
						segindex, len),
				 errhint("Every instance of the cluster must run the same build of pg_pmu.")));
	memcpy(&report, data, sizeof(report));
	sums->reports++;
	if (slice != NULL)
		slice->reports++;

	if (report.status != PMU_OK)
	{
		sums->failures++;
		sums->failure = report;
		sums->failure_segment = segindex;
		return;
	}
	sums->instructions += report.instructions;
	sums->cycles += report.cycles;
	if (slice != NULL)
	{
		slice->qes++;
		slice->instructions += report.instructions;
		slice->cycles += report.cycles;
		if (slice->qes == 1 || report.instructions > slice->max_instructions)
		{
			slice->max_instructions = report.instructions;
			slice->max_segment = segindex;
		}
	}
}

/* The sums of a slice in a record, made when its first message comes. */
static PmuSlice *
pmu_record_slice(PmuRecord *record, PmuFrame *frame, int sliceIndex)
{
	PmuSlice   *slice;

	for (int i = 0; i < record->nslices; i++)
		if (record->slices[i].slice == sliceIndex)
			return &record->slices[i];
	if (record->nslices == record->allocated)
	{
		int			allocated = Max(8, record->allocated * 2);

		if (record->slices == NULL)
			record->slices = MemoryContextAlloc(frame->cxt,
												allocated * sizeof(PmuSlice));
		else
			record->slices = repalloc(record->slices,
									  allocated * sizeof(PmuSlice));
		record->allocated = allocated;
	}
	slice = &record->slices[record->nslices++];
	memset(slice, 0, sizeof(PmuSlice));
	slice->slice = sliceIndex;
	return slice;
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
		appendStringInfoSpaces(es->str, es->indent * 2);
		appendStringInfo(es->str,
						 "%s PMU: instructions=" UINT64_FORMAT
						 " cycles=" UINT64_FORMAT "\n",
						 phase, instructions, cycles);
	}
	else
	{
		char		label[64];

		snprintf(label, sizeof(label), "%s Instructions", phase);
		ExplainPropertyInteger(label, NULL, (int64) instructions, es);
		snprintf(label, sizeof(label), "%s Cycles", phase);
		ExplainPropertyInteger(label, NULL, (int64) cycles, es);
	}
}

static int
pmu_slice_cmp(const void *a, const void *b)
{
	int			x = ((const PmuSlice *) a)->slice;
	int			y = ((const PmuSlice *) b)->slice;

	return x < y ? -1 : x > y ? 1 : 0;
}

/*
 * Under VERBOSE: the counts of each slice the QEs ran, with the most
 * instructions of one QE and its segment.
 */
static void
pmu_print_slices(ExplainState *es, PmuRecord *record)
{
	qsort(record->slices, record->nslices, sizeof(PmuSlice), pmu_slice_cmp);
	if (es->format != EXPLAIN_FORMAT_TEXT)
		ExplainOpenGroup("Slices PMU", "Slices PMU", false, es);
	for (int i = 0; i < record->nslices; i++)
	{
		PmuSlice   *s = &record->slices[i];

		if (es->format == EXPLAIN_FORMAT_TEXT)
		{
			appendStringInfoSpaces(es->str, es->indent * 2);
			appendStringInfo(es->str,
							 "Slice %d PMU: instructions=" UINT64_FORMAT
							 " cycles=" UINT64_FORMAT
							 " QEs=%d, max instructions=" UINT64_FORMAT
							 " (seg%d)\n",
							 s->slice, s->instructions, s->cycles, s->qes,
							 s->max_instructions, s->max_segment);
		}
		else
		{
			ExplainOpenGroup("Slice", NULL, true, es);
			ExplainPropertyInteger("Slice", NULL, s->slice, es);
			ExplainPropertyInteger("Instructions", NULL,
								   (int64) s->instructions, es);
			ExplainPropertyInteger("Cycles", NULL, (int64) s->cycles, es);
			ExplainPropertyInteger("QEs", NULL, s->qes, es);
			ExplainPropertyInteger("Max Instructions", NULL,
								   (int64) s->max_instructions, es);
			ExplainPropertyInteger("Max Segment", NULL, s->max_segment, es);
			ExplainCloseGroup("Slice", NULL, true, es);
		}
	}
	if (es->format != EXPLAIN_FORMAT_TEXT)
		ExplainCloseGroup("Slices PMU", "Slices PMU", false, es);
}

/*
 * Fail the statement when its counts would be short: a query inside it
 * that its QEs could not report, a QE whose statistics came without a
 * report, or a QE that could not count.
 */
static void
pmu_check_counts(PmuFrame *frame, PmuRecord *record, const PmuQeSums *nested)
{
	if (frame->lost != NULL)
		ereport(ERROR,
				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
				 errmsg("pg_pmu cannot count the whole statement"),
				 errdetail("Its counts would be short: %s.", frame->lost)));

	for (int i = 0; i < record->nslices; i++)
	{
		PmuSlice   *s = &record->slices[i];

		if (s->reports < s->messages)
			ereport(ERROR,
					(errcode(ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE),
					 errmsg("%d of the %d QEs of slice %d sent no counts",
							s->messages - s->reports, s->messages, s->slice),
					 errhint("Load pg_pmu in every backend of the cluster, "
							 "with session_preload_libraries or "
							 "shared_preload_libraries set by gpconfig.")));
	}
	if (nested->reports < nested->messages)
		ereport(ERROR,
				(errcode(ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE),
				 errmsg("%d QEs of the statements inside this one sent no counts",
						nested->messages - nested->reports),
				 errhint("Load pg_pmu in every backend of the cluster, "
						 "with session_preload_libraries or "
						 "shared_preload_libraries set by gpconfig.")));

	pmu_fail_report(&record->sums, "the statement");
	pmu_fail_report(nested, "a statement inside this one");
}

/* Fail with the reason of a QE that could not count, if there is one. */
static void
pmu_fail_report(const PmuQeSums *sums, const char *what)
{
	int32		os_error = sums->failure.os_error;

	if (sums->failures == 0)
		return;
	ereport(ERROR,
			(errcode(pmu_status_sqlstate(sums->failure.status)),
			 errmsg("a segment could not count %s", what),
			 os_error != 0 ?
			 errdetail("On seg%d: %s: %s.", sums->failure_segment,
					   pmu_status_text(sums->failure.status),
					   strerror(os_error)) :
			 errdetail("On seg%d: %s.", sums->failure_segment,
					   pmu_status_text(sums->failure.status))));
}

/*
 * The hook that runs for each plan after EXPLAIN ANALYZE has run and
 * printed it, before the executor ends: fail when the counts would be
 * short, then print the counts of its planning, if this EXPLAIN planned
 * it, and those of its execution: the QD's, its QEs' and those of the
 * QEs of statements nested in it while it ran; under VERBOSE also each
 * part. When the frame ends, the QEs' counts go down to an enclosing
 * counting EXPLAIN, whose statement this one is a part of. It does
 * nothing for an EXPLAIN without the option.
 */
static void
pmu_explain_per_plan(PlannedStmt *plannedstmt, IntoClause *into,
					 ExplainState *es, const char *queryString,
					 ParamListInfo params, QueryEnvironment *queryEnv)
{
	PmuExplainState *state;
	PmuFrame   *frame;
	PmuRecord  *record = NULL;
	PmuQeSums	nested;
	uint64		qe_instructions;
	uint64		qe_cycles;

	if (prev_explain_per_plan_hook)
		prev_explain_per_plan_hook(plannedstmt, into, es, queryString,
								   params, queryEnv);

	state = GetExplainExtensionState(es, pmu_extension_id);
	if (state == NULL || !state->pmu)
		return;

	frame = state->frame;
	if (frame != NULL)
		for (PmuRecord *r = frame->records; r != NULL; r = r->next)
			if (r->plan == plannedstmt && r->finished)
			{
				record = r;
				break;
			}
	if (record == NULL)
		ereport(ERROR,
				(errcode(ERRCODE_INTERNAL_ERROR),
				 errmsg("pg_pmu found no counts of the plan it prints")));

	/* What the statements nested in this one sent while it ran. */
	nested = pmu_sums_since(&record->nested_finish, &record->nested_start);
	pmu_check_counts(frame, record, &nested);

	/* The QEs of statements the planner ran count in the planning. */
	if (state->planned == plannedstmt)
	{
		pmu_check_counts(frame, record, &state->plan_nested);
		pmu_print(es, "Planning",
				  state->plan_end.instructions - state->plan_start.instructions +
				  state->plan_nested.instructions,
				  state->plan_end.cycles - state->plan_start.cycles +
				  state->plan_nested.cycles);
	}
	state->planned = NULL;

	qe_instructions = record->sums.instructions + nested.instructions;
	qe_cycles = record->sums.cycles + nested.cycles;
	pmu_print(es, "Execution",
			  record->finish.instructions - record->start.instructions +
			  qe_instructions,
			  record->finish.cycles - record->start.cycles + qe_cycles);
	if (es->verbose)
	{
		pmu_print(es, "Coordinator",
				  record->finish.instructions - record->start.instructions,
				  record->finish.cycles - record->start.cycles);
		pmu_print_slices(es, record);
		if (nested.messages > 0)
			pmu_print(es, "Nested QEs", nested.instructions, nested.cycles);
	}

	/* An enclosing counting statement takes them when this frame ends. */
	pmu_sums_add(&frame->handed, &record->sums);
	pmu_free_records(frame);
}

/* What a code of the crate means, for the detail of a QE's failure. */
static const char *
pmu_status_text(int32 status)
{
	switch (status)
	{
		case PMU_NOT_PERMITTED:
			return "the system does not allow the process to count";
		case PMU_NO_SYSTEM:
			return "this build of pg_pmu has no counters";
		case PMU_NO_COUNTERS:
			return "the system keeps no counters of instructions for the process";
		case PMU_CORES_NOT_COVERED:
			return "the kinds of cores in sysfs do not cover all the processors online";
		case PMU_TOO_MANY_KINDS:
			return "the processor has more kinds of cores than pg_pmu counts on";
		case PMU_TOO_MANY_THREADS:
			return "the process runs more threads than pg_pmu counts on";
		case PMU_EVENTS_NOT_COUNTED:
			return "the system cannot count the branches or the cache";
		case PMU_KIND_NOT_OPENABLE:
			return "the kernel cannot count on every kind of cores";
		case PMU_NO_FREE_COUNTER:
			return "no hardware counter is free";
		case PMU_NO_DESCRIPTOR:
			return "no file descriptor is left for the counters";
		case PMU_COUNTER_LOST:
			return "the system took a counter away while the statement ran";
		case PMU_SYSTEM_ERROR:
			return "the counters of the processor failed";
		default:
			return "internal error of pg_pmu";
	}
}
