/*-------------------------------------------------------------------------
 *
 * workers.c
 *		The cells of shared memory where the parallel workers of a
 *		counting statement report, and the setting that names them.
 *
 * The memory "pg_pmu" holds a cell for each backend and a pool of
 * pg_pmu.max_nested cells for nested statements; the crate owns its
 * layout and every step on a cell. A counting statement takes a cell,
 * the cell of its backend when it is the outermost one, and saves its
 * token in the setting pg_pmu.cell, which PostgreSQL copies into the
 * statement's workers.
 *
 * A taken cell is held apart from the statement. At the normal end of
 * the statement its workers have exited, and the cell goes back at
 * once. When the statement fails, a worker may still add to the cell
 * until PostgreSQL has waited for it, which it does in the abort of the
 * subtransaction that made the worker's context. So the cell then goes
 * back from the callback of the abort of the subtransaction in which it
 * was taken. A cell that goes back hands what it held to the hold below
 * it, the cell of the statement around.
 *
 * A worker counts its execution of the plan whose rows go to the tuple
 * queue of the Gather, and reports once into the cell of its token: at
 * the end of its finish, or from a callback of its exit when it fails
 * before. A worker that cannot count reports its failure, and the
 * statement fails at its finish even when a function caught the error.
 *
 * Copyright (c) 2026, Denis Smirnov
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#include "access/parallel.h"
#include "access/xact.h"
#include "miscadmin.h"
#include "storage/ipc.h"
#include "storage/procnumber.h"
#include "storage/shmem.h"
#include "utils/guc.h"
#include "utils/memutils.h"

#include "pg_pmu.h"
#include "pg_pmu_core.h"

/* Room for a token in decimal. */
#define PMU_TOKEN_LEN 21

struct PmuHold
{
	struct PmuHold *prev;		/* the hold of the statement around */
	uint64		token;
	SubTransactionId subid;		/* where the cell was taken */
	PmuCellContents nested;		/* what the cells of nested statements
								 * handed down */
};

/* pg_pmu.max_nested: nested counting statements the server runs at once */
static int	pmu_max_nested = 16;

/* pg_pmu.cell: the token of the cell of the counting statement, or 0 */
static char *pmu_cell = NULL;

/* The module itself is changing pg_pmu.cell. */
static bool pmu_setting_cell = false;

/* The memory "pg_pmu", mapped by PostgreSQL in each process. */
static void *pmu_cells = NULL;

/* The holds of this backend, the innermost first. */
static PmuHold *pmu_holds = NULL;

/* A worker that counts: its token, its plan and its counts at the start. */
static bool pmu_worker_counting = false;
static uint64 pmu_worker_token = 0;
static QueryDesc *pmu_worker_query = NULL;
static PmuCounts pmu_worker_counts;
static bool pmu_worker_exit_set = false;

static void pmu_shmem_request(void *arg);
static void pmu_shmem_init(void *arg);
static bool pmu_cell_check(char **newval, void **extra, GucSource source);
static void pmu_hand_down(const PmuCellContents *contents);
static void pmu_give_back_on_abort(void);
static void pmu_worker_exit(int code, Datum arg);
static void pmu_xact_callback(XactEvent event, void *arg);
static void pmu_subxact_callback(SubXactEvent event, SubTransactionId mySubid,
								 SubTransactionId parentSubid, void *arg);

static const ShmemCallbacks pmu_shmem_callbacks = {
	.request_fn = pmu_shmem_request,
	.init_fn = pmu_shmem_init,
};

/*
 * Request the memory, define the settings of the workers and register
 * the callbacks of the ends of transactions. Called from _PG_init,
 * while the server starts.
 */
void
pmu_workers_init(void)
{
	RegisterShmemCallbacks(&pmu_shmem_callbacks);

	DefineCustomIntVariable("pg_pmu.max_nested",
							"Nested counting statements the server can run at once.",
							"A counting EXPLAIN that starts while another one of "
							"the same backend runs takes one of them.",
							&pmu_max_nested,
							16, 0, 1024,
							PGC_POSTMASTER,
							0,
							NULL, NULL, NULL);

	/*
	 * A setting of the start of a connection, so that PostgreSQL refuses
	 * SET and RESET of it; the module changes it from inside, as that
	 * context allows, and PostgreSQL copies it into the workers.
	 */
	DefineCustomStringVariable("pg_pmu.cell",
							   "The cell of shared memory the workers of a counting statement report to.",
							   "For the use of pg_pmu only.",
							   &pmu_cell,
							   "0",
							   PGC_BACKEND,
							   GUC_NO_SHOW_ALL | GUC_NOT_IN_SAMPLE |
							   GUC_DISALLOW_IN_FILE,
							   pmu_cell_check, NULL, NULL);

	RegisterXactCallback(pmu_xact_callback, NULL);
	RegisterSubXactCallback(pmu_subxact_callback, NULL);
}

/*
 * The size of the memory: PostgreSQL has computed the number of backends
 * by now.
 */
static void
pmu_shmem_request(void *arg)
{
	size_t		size = pmu_cells_size((uint32) MaxBackends,
									  (uint32) pmu_max_nested);

	if (size == 0)
		ereport(ERROR,
				(errcode(ERRCODE_PROGRAM_LIMIT_EXCEEDED),
				 errmsg("pg_pmu cannot number the cells of %d backends and %d nested statements",
						MaxBackends, pmu_max_nested)));
	ShmemRequestStruct(.name = "pg_pmu",
					   .size = (ssize_t) size,
					   .ptr = &pmu_cells,
		);
}

static void
pmu_shmem_init(void *arg)
{
	int32		status = pmu_cells_init(pmu_cells, (uint32) MaxBackends,
										(uint32) pmu_max_nested);

	if (status != PMU_OK)
		pmu_report(status, 0);
}

/*
 * Only the module changes pg_pmu.cell, and PostgreSQL copies it into a
 * worker while the worker starts; a value given at the start of a
 * connection is refused.
 */
static bool
pmu_cell_check(char **newval, void **extra, GucSource source)
{
	if (pmu_setting_cell || InitializingParallelWorker ||
		source == PGC_S_DEFAULT)
		return true;
	GUC_check_errcode(ERRCODE_INVALID_PARAMETER_VALUE);
	GUC_check_errmsg("parameter \"%s\" is for the use of pg_pmu only",
					 "pg_pmu.cell");
	return false;
}

/*
 * Take a cell for a statement that starts to count, hold it, and save
 * its token in pg_pmu.cell, in a new nesting level of the settings that
 * the caller ends with pmu_return_cell; on an error the abort of the
 * transaction ends it. A parallel worker launches no workers, so it
 * takes no cell: it returns NULL and sets *guc_level to 0.
 */
PmuHold *
pmu_take_cell(int *guc_level)
{
	PmuHold    *hold;
	char		value[PMU_TOKEN_LEN];
	int32		status;

	*guc_level = 0;
	if (IsParallelWorker())
		return NULL;

	/* Allocated first, so that no cell is taken that nothing holds. */
	hold = MemoryContextAllocZero(TopMemoryContext, sizeof(PmuHold));

	if (pmu_holds == NULL)
	{
		/* Only this backend takes its cell; taken means a fault. */
		if (MyProcNumber < 0 || MyProcNumber >= MaxBackends)
			status = PMU_INTERNAL;
		else
			status = pmu_cell_take(pmu_cells, (uint32) MyProcNumber,
								   &hold->token);
		if (status != PMU_OK)
		{
			pfree(hold);
			ereport(ERROR,
					(errcode(ERRCODE_INTERNAL_ERROR),
					 errmsg("internal error of pg_pmu: the cell of this backend is not free"),
					 errdetail("An earlier fault of the module left it taken."),
					 errhint("Restart the server.")));
		}
	}
	else
	{
		status = pmu_cell_take_pool(pmu_cells, &hold->token);
		if (status != PMU_OK)
		{
			pfree(hold);
			pmu_report(status, 0);
		}
	}

	hold->prev = pmu_holds;
	hold->subid = GetCurrentSubTransactionId();
	pmu_holds = hold;

	snprintf(value, sizeof(value), UINT64_FORMAT, hold->token);
	*guc_level = NewGUCNestLevel();
	pmu_setting_cell = true;
	PG_TRY();
	{
		(void) set_config_option("pg_pmu.cell", value, PGC_BACKEND,
								 PGC_S_SESSION, GUC_ACTION_SAVE, true, 0,
								 false);
	}
	PG_FINALLY();
	{
		pmu_setting_cell = false;
	}
	PG_END_TRY();
	return hold;
}

/*
 * Give the cell of a statement back at its normal end, when all its
 * workers have exited, and end the nesting level of its setting. A
 * setting that lost the token, or a worker still inside the cell, is a
 * fault of the module and fails the statement; the cell then goes back
 * in the abort that follows.
 */
void
pmu_return_cell(PmuHold *hold, int guc_level)
{
	char		value[PMU_TOKEN_LEN];
	PmuCellContents contents;
	int32		status;

	Assert(hold == pmu_holds);
	snprintf(value, sizeof(value), UINT64_FORMAT, hold->token);
	if (pmu_cell == NULL || strcmp(pmu_cell, value) != 0)
		ereport(ERROR,
				(errcode(ERRCODE_INTERNAL_ERROR),
				 errmsg("internal error of pg_pmu: the setting pg_pmu.cell lost the token of the statement")));

	status = pmu_cell_give_back(pmu_cells, hold->token, &contents);
	if (status != PMU_OK)
		pmu_report(status, 0);
	pmu_hand_down(&contents);
	AtEOXact_GUC(true, guc_level);
}

/*
 * Hand what the innermost cell held, and what was handed down to it,
 * to the hold below, and drop the innermost hold. The outermost hands
 * it to nobody.
 */
static void
pmu_hand_down(const PmuCellContents *contents)
{
	PmuHold    *hold = pmu_holds;
	PmuHold    *below = hold->prev;

	if (below != NULL)
	{
		below->nested.instructions += contents->instructions +
			hold->nested.instructions;
		below->nested.cycles += contents->cycles + hold->nested.cycles;
		below->nested.failures += contents->failures + hold->nested.failures;
		if (hold->nested.sqlstate != 0)
			below->nested.sqlstate = hold->nested.sqlstate;
		if (contents->sqlstate != 0)
			below->nested.sqlstate = contents->sqlstate;
	}
	pmu_holds = below;
	pfree(hold);
}

/*
 * Give the innermost cell back in the abort of a transaction, where no
 * error may be raised. A worker still inside is a fault of the module:
 * the cell stays taken until the server restarts, and the log says so.
 */
static void
pmu_give_back_on_abort(void)
{
	PmuCellContents contents = {0};
	int32		status = pmu_cell_give_back(pmu_cells, pmu_holds->token,
											&contents);

	if (status != PMU_OK)
		ereport(WARNING,
				(errcode(ERRCODE_INTERNAL_ERROR),
				 errmsg("internal error of pg_pmu: a cell of its shared memory stays taken"),
				 errdetail("The crate refused to give it back with code %d.",
						   status),
				 pmu_holds->prev == NULL ?
				 errhint("Counting statements of backends with this number fail until the server restarts.") :
				 errhint("The pool of nested statements has one cell less until the server restarts.")));
	pmu_hand_down(&contents);
}

/*
 * What the cell of a hold holds, with what nested cells handed down to
 * it; zeros without a hold. Read at the start and at the finish of a
 * candidate for the statement, the difference is the part of the
 * workers.
 */
void
pmu_hold_read(PmuHold *hold, PmuCellContents *out)
{
	int32		status;

	memset(out, 0, sizeof(*out));
	if (hold == NULL)
		return;
	status = pmu_cell_read(pmu_cells, hold->token, out);
	if (status != PMU_OK)
		pmu_report(status, 0);
	out->instructions += hold->nested.instructions;
	out->cycles += hold->nested.cycles;
	out->failures += hold->nested.failures;
	if (out->sqlstate == 0)
		out->sqlstate = hold->nested.sqlstate;
}

/*
 * The start of a worker's execution of the plan: its rows go to the
 * tuple queue of the Gather. Other queries of the worker, and the
 * workers of a parallel index build, are not counted. A worker whose
 * statement counts takes its token from pg_pmu.cell, opens its counters
 * and takes a reference to them; when it cannot, it reports its failure
 * and raises the error.
 */
void
pmu_worker_start(QueryDesc *queryDesc)
{
	uint64		token;
	int32		status;

	if (!IsParallelWorker() || pmu_worker_counting ||
		queryDesc->dest == NULL || queryDesc->dest->mydest != DestTupleQueue)
		return;

	status = pmu_token_parse(pmu_cells, pmu_cell, &token);
	if (status != PMU_OK)
		pmu_report(status, 0);
	if (token == 0)
		return;

	PG_TRY();
	{
		int32		os_error = 0;

		pmu_open_counters();
		status = pmu_counters_acquire(&pmu_worker_counts, &os_error);
		if (status != PMU_OK)
			pmu_report(status, os_error);
	}
	PG_CATCH();
	{
		(void) pmu_cell_report_failure(pmu_cells, token,
									   (uint32) geterrcode());
		PG_RE_THROW();
	}
	PG_END_TRY();

	if (!pmu_worker_exit_set)
	{
		before_shmem_exit(pmu_worker_exit, (Datum) 0);
		pmu_worker_exit_set = true;
	}
	pmu_worker_token = token;
	pmu_worker_query = queryDesc;
	pmu_worker_counting = true;
}

/*
 * The finish of the worker's execution of the plan: report the counts
 * once and give the reference back. A report that the cell refuses
 * fails the worker.
 */
void
pmu_worker_finish(QueryDesc *queryDesc)
{
	PmuCounts	now;
	int32		os_error = 0;
	int32		status;

	if (!pmu_worker_counting || queryDesc != pmu_worker_query)
		return;
	pmu_worker_counting = false;

	status = pmu_counters_read(&now, &os_error);
	if (status != PMU_OK)
	{
		(void) pmu_cell_report_failure(pmu_cells, pmu_worker_token,
									   (uint32) pmu_status_sqlstate(status));
		pmu_report(status, os_error);
	}
	status = pmu_cell_report(pmu_cells, pmu_worker_token,
							 now.instructions - pmu_worker_counts.instructions,
							 now.cycles - pmu_worker_counts.cycles);
	if (status != PMU_OK)
		pmu_report(status, 0);
	status = pmu_counters_release(&os_error);
	if (status != PMU_OK)
		pmu_report(status, os_error);
}

/*
 * A worker that fails before its finish reports from its exit what it
 * counted until then, or that it could not read its counters. No error
 * may be raised here, so a refused report goes to the log.
 */
static void
pmu_worker_exit(int code, Datum arg)
{
	PmuCounts	now;
	int32		os_error = 0;
	int32		status;

	if (!pmu_worker_counting)
		return;
	pmu_worker_counting = false;

	status = pmu_counters_read(&now, &os_error);
	if (status == PMU_OK)
		status = pmu_cell_report(pmu_cells, pmu_worker_token,
								 now.instructions - pmu_worker_counts.instructions,
								 now.cycles - pmu_worker_counts.cycles);
	else
		status = pmu_cell_report_failure(pmu_cells, pmu_worker_token,
										 (uint32) pmu_status_sqlstate(status));
	if (status != PMU_OK)
		ereport(WARNING,
				(errcode(ERRCODE_INTERNAL_ERROR),
				 errmsg("internal error of pg_pmu: the cell refused the report of a worker that failed"),
				 errdetail("The crate returned code %d.", status)));
}

/*
 * The end of the transaction gives back every cell still held: those of
 * statements that failed. At a commit none should be left; one that is
 * shows a fault of the module, and the log says so.
 */
static void
pmu_xact_callback(XactEvent event, void *arg)
{
	switch (event)
	{
		case XACT_EVENT_COMMIT:
		case XACT_EVENT_PARALLEL_COMMIT:
		case XACT_EVENT_PREPARE:
			if (pmu_holds != NULL)
				ereport(WARNING,
						(errcode(ERRCODE_INTERNAL_ERROR),
						 errmsg("internal error of pg_pmu: a cell of a statement was still held at the end of its transaction")));
			pg_fallthrough;
		case XACT_EVENT_ABORT:
		case XACT_EVENT_PARALLEL_ABORT:
			while (pmu_holds != NULL)
				pmu_give_back_on_abort();
			break;
		default:
			break;
	}
}

/*
 * The abort of a subtransaction has waited for the workers of the
 * contexts made in it, so the cells taken in it go back, the innermost
 * first, each into a hold that is still there. The cells of deeper
 * subtransactions went back in their own aborts. At the commit of a
 * subtransaction its holds pass to its parent.
 */
static void
pmu_subxact_callback(SubXactEvent event, SubTransactionId mySubid,
					 SubTransactionId parentSubid, void *arg)
{
	if (event == SUBXACT_EVENT_ABORT_SUB)
	{
		while (pmu_holds != NULL && pmu_holds->subid == mySubid)
			pmu_give_back_on_abort();
	}
	else if (event == SUBXACT_EVENT_COMMIT_SUB)
	{
		for (PmuHold *h = pmu_holds; h != NULL && h->subid == mySubid;
			 h = h->prev)
			h->subid = parentSubid;
	}
}
