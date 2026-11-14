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

#include "commands/defrem.h"
#include "commands/explain_state.h"
#include "fmgr.h"
#include "miscadmin.h"
#include "utils/guc.h"

PG_MODULE_MAGIC_EXT(.name = "pg_pmu",.version = "0.1.0");

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

static explain_validate_options_hook_type prev_validate_options_hook = NULL;

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
}
