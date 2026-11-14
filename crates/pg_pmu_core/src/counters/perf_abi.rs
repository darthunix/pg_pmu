//! The few declarations of Linux's `<linux/perf_event.h>` the crate
//! uses. `libc` has the system call but not these. A wrong offset or bit
//! would count something else without a sign, so the module compares
//! them with the kernel's header when it loads (`test/abi.c`).

/// `struct perf_event_attr` as of `PERF_ATTR_SIZE_VER0`, the first 64
/// bytes, which every kernel takes; later fields stay zero.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PerfEventAttr {
    pub type_: u32,
    pub size: u32,
    pub config: u64,
    pub sample_period: u64,
    pub sample_type: u64,
    pub read_format: u64,
    /// The bit fields of the C structure, from `disabled` at bit 0.
    pub flags: u64,
    pub wakeup_events: u32,
    pub bp_type: u32,
    pub config1: u64,
}

pub const PERF_ATTR_SIZE_VER0: u32 = 64;

pub const PERF_TYPE_HARDWARE: u32 = 0;
pub const PERF_COUNT_HW_CPU_CYCLES: u64 = 0;
pub const PERF_COUNT_HW_INSTRUCTIONS: u64 = 1;
/// The type of the unit of a kind of cores goes in the high half of
/// `config` of a hardware event, since Linux 5.13 on Intel.
pub const PERF_PMU_TYPE_SHIFT: u32 = 32;

pub const PERF_FORMAT_GROUP: u64 = 1 << 3;

pub const FLAG_DISABLED: u64 = 1 << 0;
pub const FLAG_PINNED: u64 = 1 << 2;
pub const FLAG_EXCLUDE_KERNEL: u64 = 1 << 5;
pub const FLAG_EXCLUDE_HV: u64 = 1 << 6;

pub const PERF_FLAG_FD_CLOEXEC: u64 = 1 << 3;

/// `_IO('$', 0)` and `_IO('$', 1)`, the same on x86-64 and AArch64.
pub const PERF_EVENT_IOC_ENABLE: u64 = 0x2400;
pub const PERF_EVENT_IOC_DISABLE: u64 = 0x2401;
/// Apply an `ioctl` to the whole group of a leader.
pub const PERF_IOC_FLAG_GROUP: u64 = 1;

/// The flags of the leader of a group: opened disabled, kept on the
/// hardware all the time or put into an error state, user space only.
pub const LEADER_FLAGS: u64 = FLAG_DISABLED | FLAG_PINNED | FLAG_EXCLUDE_KERNEL | FLAG_EXCLUDE_HV;
/// The flags of a member: it follows its leader, which only a leader
/// may be pinned.
pub const MEMBER_FLAGS: u64 = FLAG_EXCLUDE_KERNEL | FLAG_EXCLUDE_HV;

/// The attributes of an event of a group: `event` is the hardware
/// event, `unit` the type of a kind of cores or `None` for any.
pub const fn attr(event: u64, unit: Option<u32>, leader: bool) -> PerfEventAttr {
    let config = match unit {
        Some(t) => event | ((t as u64) << PERF_PMU_TYPE_SHIFT),
        None => event,
    };
    PerfEventAttr {
        type_: PERF_TYPE_HARDWARE,
        size: PERF_ATTR_SIZE_VER0,
        config,
        sample_period: 0,
        sample_type: 0,
        read_format: PERF_FORMAT_GROUP,
        flags: if leader { LEADER_FLAGS } else { MEMBER_FLAGS },
        wakeup_events: 0,
        bp_type: 0,
        config1: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    #[test]
    fn layout_is_that_of_version_0() {
        assert_eq!(size_of::<PerfEventAttr>(), PERF_ATTR_SIZE_VER0 as usize);
        assert_eq!(offset_of!(PerfEventAttr, config), 8);
        assert_eq!(offset_of!(PerfEventAttr, read_format), 32);
        assert_eq!(offset_of!(PerfEventAttr, flags), 40);
        assert_eq!(offset_of!(PerfEventAttr, config1), 56);
    }

    #[test]
    fn unit_goes_in_the_high_half() {
        let a = attr(PERF_COUNT_HW_INSTRUCTIONS, Some(8), true);
        assert_eq!(a.config, (8u64 << 32) | 1);
        assert_eq!(attr(PERF_COUNT_HW_CPU_CYCLES, None, false).config, 0);
    }
}
