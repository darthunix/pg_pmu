/*-------------------------------------------------------------------------
 *
 * abi.c
 *		Checks of the boundary between the module and the Rust crate.
 *
 * pg_pmu_core.h is written by hand, so the module compares its types
 * with the crate's when it loads, and refuses to start on a mismatch
 * rather than read counts at the wrong offsets.
 *
 * Copyright (c) 2026, Denis Smirnov
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#include <stddef.h>

#include "../pg_pmu_core.h"

void		pmu_check_layout(void);

/*
 * Compare the layout of the shared types with the crate's.
 */
void
pmu_check_layout(void)
{
	PmuLayout	layout = {0};

	pmu_core_layout(&layout);
	if (layout.counts_size != sizeof(PmuCounts) ||
		layout.counts_cycles != offsetof(PmuCounts, cycles))
		ereport(ERROR,
				(errcode(ERRCODE_INTERNAL_ERROR),
				 errmsg("pg_pmu_core.h does not match the crate pg_pmu_core"),
				 errdetail("PmuCounts has %u bytes and cycles at %u in the crate, "
						   "%zu and %zu in the header.",
						   layout.counts_size, layout.counts_cycles,
						   sizeof(PmuCounts), offsetof(PmuCounts, cycles))));
}
