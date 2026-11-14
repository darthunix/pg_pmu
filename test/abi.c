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
#include <string.h>

#ifdef __linux__
#include <linux/perf_event.h>
#endif

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
		layout.counts_cycles != offsetof(PmuCounts, cycles) ||
		layout.perf_abi_size != sizeof(PmuPerfAbi) ||
		layout.contents_size != sizeof(PmuCellContents) ||
		layout.contents_failures != offsetof(PmuCellContents, failures))
		ereport(ERROR,
				(errcode(ERRCODE_INTERNAL_ERROR),
				 errmsg("pg_pmu_core.h does not match the crate pg_pmu_core"),
				 errdetail("PmuCounts has %u bytes and cycles at %u, PmuPerfAbi "
						   "%u bytes, PmuCellContents %u bytes and failures at "
						   "%u in the crate; %zu, %zu, %zu, %zu and %zu in the "
						   "header.",
						   layout.counts_size, layout.counts_cycles,
						   layout.perf_abi_size, layout.contents_size,
						   layout.contents_failures, sizeof(PmuCounts),
						   offsetof(PmuCounts, cycles), sizeof(PmuPerfAbi),
						   sizeof(PmuCellContents),
						   offsetof(PmuCellContents, failures))));
}

#ifdef __linux__

#ifndef PERF_PMU_TYPE_SHIFT
/* Linux 5.13 and later; older headers do not name it. */
#define PERF_PMU_TYPE_SHIFT 32
#endif

/* The word of bit fields of a perf_event_attr made with these flags. */
static uint64_t
flags_word(const PmuPerfAbi *abi, bool leader)
{
	struct perf_event_attr attr;
	uint64_t	word;

	memset(&attr, 0, sizeof(attr));
	attr.disabled = leader;
	attr.pinned = leader;
	attr.exclude_kernel = 1;
	attr.exclude_hv = 1;
	memcpy(&word, (const char *) &attr + abi->flags_offset, sizeof(word));
	return word;
}

#define PMU_CHECK(rust, header) \
	do { \
		if ((uint64_t) (rust) != (uint64_t) (header)) \
			ereport(ERROR, \
					(errcode(ERRCODE_INTERNAL_ERROR), \
					 errmsg("the crate pg_pmu_core does not match <linux/perf_event.h>"), \
					 errdetail("%s is %llu in the crate and %llu in the header.", \
							   #header, (unsigned long long) (rust), \
							   (unsigned long long) (header)))); \
	} while (0)
#endif

void		pmu_check_perf_abi(void);

/*
 * On Linux, compare what the crate passes to the kernel with the
 * kernel's own header. A wrong offset or bit would count something else
 * without a sign. Elsewhere there is nothing to compare.
 */
void
pmu_check_perf_abi(void)
{
#ifdef __linux__
	PmuPerfAbi	abi = {0};

	pmu_core_perf_abi(&abi);
	PMU_CHECK(abi.attr_size, PERF_ATTR_SIZE_VER0);
	PMU_CHECK(abi.config_offset, offsetof(struct perf_event_attr, config));
	PMU_CHECK(abi.read_format_offset, offsetof(struct perf_event_attr, read_format));
	/* The bit fields are the word after read_format. */
	PMU_CHECK(abi.flags_offset, offsetof(struct perf_event_attr, read_format) + 8);
	PMU_CHECK(abi.type_hardware, PERF_TYPE_HARDWARE);
	PMU_CHECK(abi.pmu_type_shift, PERF_PMU_TYPE_SHIFT);
	PMU_CHECK(abi.hw_instructions, PERF_COUNT_HW_INSTRUCTIONS);
	PMU_CHECK(abi.hw_cycles, PERF_COUNT_HW_CPU_CYCLES);
	PMU_CHECK(abi.format_group, PERF_FORMAT_GROUP);
	PMU_CHECK(abi.leader_flags, flags_word(&abi, true));
	PMU_CHECK(abi.member_flags, flags_word(&abi, false));
	PMU_CHECK(abi.flag_fd_cloexec, PERF_FLAG_FD_CLOEXEC);
	PMU_CHECK(abi.ioc_enable, PERF_EVENT_IOC_ENABLE);
	PMU_CHECK(abi.ioc_disable, PERF_EVENT_IOC_DISABLE);
	PMU_CHECK(abi.ioc_flag_group, PERF_IOC_FLAG_GROUP);
#endif
}
