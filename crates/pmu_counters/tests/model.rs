//! The crate against plain models, with random cases from proptest:
//! the rule of the kinds of cores over random machines in sysfs, the
//! references to the counters, and the groups of perf events over a
//! system that fails where the case says.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::{BTreeSet, HashMap};

use proptest::prelude::*;

use pmu_counters::counters::perf::{Perf, PerfSys, open_error};
use pmu_counters::counters::perf_abi::{
    LEADER_FLAGS, MEMBER_FLAGS, PERF_EVENT_IOC_DISABLE, PERF_EVENT_IOC_ENABLE, PerfEventAttr,
};
use pmu_counters::counters::{Backend, Counters, Counts};
use pmu_counters::sysfs::{self, Fs, MAX_KINDS, Malformed, Plan};
use pmu_counters::{Error, Status};

// ---------------------------------------------------------------------
// The kinds of cores
// ---------------------------------------------------------------------

/// A sysfs in memory.
#[derive(Default, Debug)]
struct MemFs {
    files: HashMap<Vec<u8>, Vec<u8>>,
    dirs: HashMap<Vec<u8>, Vec<Vec<u8>>>,
}

impl Fs for MemFs {
    fn read(&self, path: &[u8], buf: &mut [u8]) -> Result<usize, Malformed> {
        let file = self.files.get(path).ok_or(Malformed)?;
        if file.len() >= buf.len() {
            return Err(Malformed);
        }
        buf[..file.len()].copy_from_slice(file);
        Ok(file.len())
    }

    fn list(&self, dir: &[u8], each: &mut dyn FnMut(&[u8])) -> Result<(), Malformed> {
        for name in self.dirs.get(dir).ok_or(Malformed)? {
            each(name);
        }
        Ok(())
    }
}

/// A unit of a kind of cores.
#[derive(Clone, Debug)]
struct Unit {
    cpus: BTreeSet<u32>,
    /// None: the file type cannot be read.
    unit_type: Option<u32>,
}

#[derive(Clone, Debug)]
struct Machine {
    online: BTreeSet<u32>,
    units: Vec<Unit>,
    online_readable: bool,
    listable: bool,
    uniform: Option<bool>,
    /// How each list is cut into ranges: a seed for each.
    cuts: Vec<u64>,
    newline: bool,
}

/// Write a set of processors as the kernel does, in ranges, cut in more
/// pieces where the seed says.
fn render(set: &BTreeSet<u32>, seed: u64, newline: bool) -> Vec<u8> {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    let mut s = seed;
    for &c in set {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        match ranges.last_mut() {
            Some((_, last)) if *last + 1 == c && s >> 62 != 0 => *last = c,
            _ => ranges.push((c, c)),
        }
    }
    let mut text = ranges
        .iter()
        .map(|&(a, b)| {
            if a == b {
                format!("{a}")
            } else {
                format!("{a}-{b}")
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    if newline {
        text.push('\n');
    }
    text.into_bytes()
}

fn machine() -> impl Strategy<Value = Machine> {
    (
        prop::collection::btree_set(0u32..96, 1..40),
        0usize..=MAX_KINDS + 1,
        any::<u64>(),
        prop::bool::weighted(0.95),
        prop::bool::weighted(0.95),
        prop::option::of(any::<bool>()),
        any::<bool>(),
    )
        .prop_flat_map(
            |(online, kinds, seed, online_readable, listable, uniform, newline)| {
                let n = online.len();
                (
                    Just(online),
                    // The kind of each online processor; kinds means none.
                    prop::collection::vec(0..=kinds, n),
                    prop::collection::vec(prop::option::weighted(0.9, 1u32..64), kinds),
                    // Processors offline that a unit lists too.
                    prop::collection::vec(prop::collection::btree_set(96u32..128, 0..3), kinds),
                    Just((seed, online_readable, listable, uniform, newline, kinds)),
                )
            },
        )
        .prop_map(
            |(
                online,
                owner,
                types,
                extra,
                (seed, online_readable, listable, uniform, newline, kinds),
            )| {
                let mut units: Vec<Unit> = (0..kinds)
                    .map(|k| Unit {
                        cpus: extra[k].clone(),
                        unit_type: types[k],
                    })
                    .collect();
                for (&cpu, &k) in online.iter().zip(owner.iter()) {
                    if k < kinds {
                        units[k].cpus.insert(cpu);
                    }
                }
                Machine {
                    online,
                    units,
                    online_readable,
                    listable,
                    uniform,
                    cuts: (0..=kinds as u64)
                        .map(|i| seed ^ i.wrapping_mul(0x9e3779b97f4a7c15))
                        .collect(),
                    newline,
                }
            },
        )
}

const DEVICES: &[u8] = b"bus/event_source/devices";

fn sysfs_of(m: &Machine) -> MemFs {
    let mut fs = MemFs::default();
    // Units that are not kinds of cores: with no file cpus, or one that
    // does not read as a list, beside a type.
    let mut names: Vec<Vec<u8>> = vec![b"software".to_vec(), b"tracepoint".to_vec()];
    for (name, cpus) in [(&b"broken"[..], &b"0-x\n"[..]), (b"reversed", b"5-2\n")] {
        fs.files
            .insert([DEVICES, b"/", name, b"/cpus"].concat(), cpus.to_vec());
        fs.files
            .insert([DEVICES, b"/", name, b"/type"].concat(), b"99\n".to_vec());
        names.push(name.to_vec());
    }
    for (k, unit) in m.units.iter().enumerate() {
        let name = format!("cpu_kind{k}").into_bytes();
        let path = |file: &str| [DEVICES, b"/", &name, b"/", file.as_bytes()].concat();
        fs.files
            .insert(path("cpus"), render(&unit.cpus, m.cuts[k], m.newline));
        if let Some(t) = unit.unit_type {
            fs.files.insert(path("type"), format!("{t}\n").into_bytes());
        }
        names.push(name);
    }
    if m.listable {
        fs.dirs.insert(DEVICES.to_vec(), names);
    }
    if m.online_readable {
        fs.files.insert(
            b"devices/system/cpu/online".to_vec(),
            render(&m.online, *m.cuts.last().unwrap(), m.newline),
        );
    }
    fs
}

/// The rule, written plainly.
fn model(m: &Machine) -> Result<Plan, Status> {
    let units: Vec<&Unit> = if m.listable {
        m.units.iter().filter(|u| u.unit_type.is_some()).collect()
    } else {
        Vec::new()
    };
    if units.len() > MAX_KINDS {
        return Err(Status::TooManyKinds);
    }
    let mut plan = Plan {
        typed: false,
        count: 0,
        types: [0; MAX_KINDS],
    };
    if units.is_empty() {
        return match m.uniform {
            Some(true) => Ok(Plan { count: 1, ..plan }),
            Some(false) => Err(Status::CoresNotCovered),
            None => Err(Status::NoCounters),
        };
    }
    if !m.online_readable {
        return Err(Status::CoresNotCovered);
    }
    let covered: BTreeSet<u32> = units.iter().flat_map(|u| u.cpus.iter().copied()).collect();
    if !m.online.is_subset(&covered) {
        return Err(Status::CoresNotCovered);
    }
    for (slot, u) in plan.types.iter_mut().zip(&units) {
        *slot = u.unit_type.unwrap();
    }
    plan.typed = units.len() > 1;
    plan.count = units.len();
    Ok(plan)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn kinds_of_cores_follow_the_rule(m in machine()) {
        let fs = sysfs_of(&m);
        prop_assert_eq!(sysfs::discover(&fs, m.uniform), model(&m), "{:?}", m);
    }

    #[test]
    fn lists_of_processors_read_back(set in prop::collection::btree_set(0u32..8192, 0..64),
                                     seed in any::<u64>(), newline in any::<bool>()) {
        let text = render(&set, seed, newline);
        let parsed = sysfs::parse_cpu_list::<128>(&text).unwrap();
        let mut expected = sysfs::CpuSet::<128>::new();
        for c in &set {
            expected.insert_range(*c, *c).unwrap();
        }
        prop_assert_eq!(parsed, expected);
    }

    #[test]
    fn broken_lists_are_refused(text in "[0-9,\\-a ]{0,12}") {
        // What the kernel never writes: a valid list is digits, and
        // ranges of two numbers in order, separated by single commas.
        let valid = {
            let t = text.trim_end();
            t.is_empty() || t.split(',').all(|item| {
                let ends: Vec<&str> = item.splitn(2, '-').collect();
                ends.iter().all(|e| !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit()))
                    && ends.iter().all(|e| e.parse::<u32>().is_ok_and(|n| n < 8192))
                    && (ends.len() == 1 || ends[0].parse::<u32>().unwrap() <= ends[1].parse::<u32>().unwrap())
            })
        };
        prop_assert_eq!(sysfs::parse_cpu_list::<128>(text.as_bytes()).is_ok(), valid, "{:?}", text);
    }
}

// ---------------------------------------------------------------------
// The references
// ---------------------------------------------------------------------

/// A backend that fails a call when told, and counts while enabled.
#[derive(Default)]
struct Fake {
    enabled: bool,
    value: u64,
    fail_next: bool,
}

impl Fake {
    fn fail(&mut self) -> Result<(), Error> {
        if std::mem::take(&mut self.fail_next) {
            Err(Error::new(Status::CounterLost))
        } else {
            Ok(())
        }
    }
}

impl Backend for Fake {
    fn plan(&mut self, _: &std::ffi::CStr) -> Result<u32, Error> {
        Ok(0)
    }
    fn open(&mut self) -> Result<(), Error> {
        Ok(())
    }
    fn enable(&mut self) -> Result<(), Error> {
        self.fail()?;
        self.enabled = true;
        Ok(())
    }
    fn disable(&mut self) -> Result<(), Error> {
        self.enabled = false;
        Ok(())
    }
    fn read(&mut self) -> Result<Counts, Error> {
        self.fail()?;
        if self.enabled {
            self.value += 1;
        }
        Ok(Counts {
            instructions: self.value,
            cycles: self.value,
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Acquire { fail: bool },
    Read { fail: bool },
    Release,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        prop::bool::weighted(0.1).prop_map(|fail| Op::Acquire { fail }),
        prop::bool::weighted(0.1).prop_map(|fail| Op::Read { fail }),
        Just(Op::Release),
    ]
}

proptest! {
    #[test]
    fn references_follow_the_model(ops in prop::collection::vec(op(), 0..64)) {
        let mut c = Counters::new(Fake::default());
        c.open().unwrap();
        let mut refs = 0u32;
        // The fake counts one at each read while it is enabled.
        let mut value = 0u64;
        for op in ops {
            match op {
                Op::Acquire { fail } => {
                    c_fail(&mut c, fail);
                    let r = c.acquire();
                    prop_assert_eq!(r.is_ok(), !fail);
                    if let Ok(counts) = r {
                        refs += 1;
                        value += 1;
                        prop_assert_eq!(counts.instructions, value);
                    }
                }
                Op::Read { fail } => {
                    c_fail(&mut c, fail);
                    let r = c.read();
                    if refs == 0 {
                        prop_assert_eq!(r.unwrap_err().status, Status::Internal);
                        clear_fail(&mut c);
                    } else {
                        prop_assert_eq!(r.is_ok(), !fail);
                        if let Ok(counts) = r {
                            value += 1;
                            prop_assert_eq!(counts.instructions, value);
                        }
                    }
                }
                Op::Release => {
                    let r = c.release();
                    if refs == 0 {
                        prop_assert_eq!(r.unwrap_err().status, Status::Internal);
                    } else {
                        prop_assert!(r.is_ok());
                        refs -= 1;
                    }
                }
            }
            prop_assert_eq!(c.refs(), refs);
            prop_assert_eq!(c.backend().enabled, refs > 0);
        }
    }
}

fn c_fail(c: &mut Counters<Fake>, fail: bool) {
    c.backend_mut().fail_next = fail;
}

fn clear_fail(c: &mut Counters<Fake>) {
    c.backend_mut().fail_next = false;
}

// ---------------------------------------------------------------------
// The groups of perf events
// ---------------------------------------------------------------------

/// A system of perf events in memory, that fails one call when told.
#[derive(Default, Debug)]
struct FakeSys {
    next_fd: i32,
    open: BTreeSet<i32>,
    /// The leaders of groups, which reads and ioctls address.
    leaders: BTreeSet<i32>,
    enabled: BTreeSet<i32>,
    /// Fail the open with this index with this errno.
    fail_open: Option<(usize, i32)>,
    opens: usize,
    /// Reads of an enabled leader give no bytes: no free counter.
    error_state: bool,
    /// Each open: its attributes and the group it joins.
    opened: Vec<(PerfEventAttr, i32)>,
    /// Each ioctl: its descriptor, request and argument.
    ioctls: Vec<(i32, u64, u64)>,
    /// What reads of instructions give, or the descriptor if None.
    instructions: Option<u64>,
    /// Fail an enable of this leader.
    fail_enable_of: Option<i32>,
}

impl PerfSys for FakeSys {
    fn open(&mut self, attr: &PerfEventAttr, group_fd: i32) -> Result<i32, i32> {
        let index = self.opens;
        self.opens += 1;
        if let Some((i, e)) = self.fail_open
            && i == index
        {
            return Err(e);
        }
        assert!(group_fd == -1 || self.leaders.contains(&group_fd));
        self.opened.push((*attr, group_fd));
        self.next_fd += 1;
        let fd = self.next_fd + 100;
        self.open.insert(fd);
        if group_fd == -1 {
            self.leaders.insert(fd);
        }
        Ok(fd)
    }

    fn ioctl(&mut self, fd: i32, request: u64, arg: u64) -> Result<(), i32> {
        assert!(self.leaders.contains(&fd));
        self.ioctls.push((fd, request, arg));
        match request {
            PERF_EVENT_IOC_ENABLE => {
                if self.fail_enable_of == Some(fd) {
                    return Err(libc::EIO);
                }
                self.enabled.insert(fd);
            }
            PERF_EVENT_IOC_DISABLE => {
                self.enabled.remove(&fd);
            }
            _ => panic!("ioctl {request:#x}"),
        }
        Ok(())
    }

    fn read(&mut self, fd: i32, buf: &mut [u8]) -> Result<usize, i32> {
        assert!(self.leaders.contains(&fd));
        if self.error_state && self.enabled.contains(&fd) {
            return Ok(0);
        }
        let instructions = self.instructions.unwrap_or(1000 + fd as u64);
        let words = [2u64, instructions, 2000 + fd as u64];
        for (i, w) in words.iter().enumerate() {
            buf[i * 8..i * 8 + 8].copy_from_slice(&w.to_ne_bytes());
        }
        Ok(24)
    }

    fn close(&mut self, fd: i32) {
        assert!(self.open.remove(&fd), "closed twice: {fd}");
        self.leaders.remove(&fd);
        self.enabled.remove(&fd);
    }

    fn uniform_cores(&self) -> Option<bool> {
        Some(true)
    }
}

fn plan_of(count: usize, typed: bool) -> Plan {
    Plan {
        typed,
        count,
        types: [8, 9, 10, 11],
    }
}

const ERRNOS: [i32; 8] = [
    libc::EACCES,
    libc::EPERM,
    libc::ENOENT,
    libc::EOPNOTSUPP,
    libc::EINVAL,
    libc::EMFILE,
    libc::EBUSY,
    libc::EIO,
];

proptest! {
    #[test]
    fn a_failed_opening_closes_what_it_opened(count in 1usize..=MAX_KINDS, typed in any::<bool>(),
                                              at in 0usize..8, errno in prop::sample::select(&ERRNOS[..])) {
        let typed = typed && count > 1;
        let mut perf = Perf::with(FakeSys { fail_open: Some((at, errno)), ..FakeSys::default() });
        prop_assert_eq!(perf.set_plan(plan_of(count, typed)), 2 * count as u32);
        let r = perf.open();
        if at < 2 * count {
            prop_assert_eq!(r, Err(open_error(errno, typed)));
            prop_assert!(perf.sys().open.is_empty(), "left open: {:?}", perf.sys().open);
        } else {
            prop_assert_eq!(r, Ok(()));
            prop_assert_eq!(perf.sys().open.len(), 2 * count);
            prop_assert!(perf.sys().enabled.is_empty(), "the check leaves them disabled");
        }
    }
}

#[test]
fn a_next_opening_succeeds_after_a_failure() {
    let mut perf = Perf::with(FakeSys {
        fail_open: Some((3, libc::EMFILE)),
        ..FakeSys::default()
    });
    perf.set_plan(plan_of(2, true));
    assert_eq!(perf.open().unwrap_err().status, Status::NoDescriptor);
    assert!(perf.sys().open.is_empty());
    perf.sys_mut().fail_open = None;
    perf.open().unwrap();
    assert_eq!(perf.sys().open.len(), 4);
}

#[test]
fn leaders_count_instructions_and_members_cycles_of_their_kind() {
    use pmu_counters::counters::perf_abi::{
        PERF_COUNT_HW_CPU_CYCLES, PERF_COUNT_HW_INSTRUCTIONS, PERF_FORMAT_GROUP,
        PERF_IOC_FLAG_GROUP, PERF_TYPE_HARDWARE,
    };
    for typed in [false, true] {
        let count = if typed { 2 } else { 1 };
        let mut perf = Perf::with(FakeSys::default());
        perf.set_plan(plan_of(count, typed));
        perf.open().unwrap();
        let opened = &perf.sys().opened;
        assert_eq!(opened.len(), 2 * count);
        for (g, pair) in opened.chunks(2).enumerate() {
            let unit = if typed { (8 + g as u64) << 32 } else { 0 };
            let (leader, member) = (&pair[0], &pair[1]);
            assert_eq!(leader.0.config, PERF_COUNT_HW_INSTRUCTIONS | unit);
            assert_eq!(member.0.config, PERF_COUNT_HW_CPU_CYCLES | unit);
            assert_eq!(leader.0.flags, LEADER_FLAGS);
            assert_eq!(member.0.flags, MEMBER_FLAGS);
            for (attr, _) in pair {
                assert_eq!(attr.type_, PERF_TYPE_HARDWARE);
                assert_eq!(attr.read_format, PERF_FORMAT_GROUP);
                assert_eq!(attr.size, 64);
            }
            // The leader opens alone; the member joins its group.
            assert_eq!(leader.1, -1);
            assert_eq!(member.1, 100 + 1 + 2 * g as i32);
        }
        // Every enable and disable applies to the whole group.
        assert!(!perf.sys().ioctls.is_empty());
        assert!(
            perf.sys()
                .ioctls
                .iter()
                .all(|&(_, _, arg)| arg == PERF_IOC_FLAG_GROUP)
        );
    }
}

#[test]
fn a_group_that_counts_nothing_is_refused() {
    let mut perf = Perf::with(FakeSys {
        instructions: Some(0),
        ..FakeSys::default()
    });
    perf.set_plan(plan_of(1, false));
    assert_eq!(perf.open().unwrap_err().status, Status::NoCounters);
    assert!(perf.sys().open.is_empty());
}

#[test]
fn a_failed_enable_leaves_no_group_on() {
    let mut perf = Perf::with(FakeSys::default());
    perf.set_plan(plan_of(2, true));
    perf.open().unwrap();
    perf.sys_mut().fail_enable_of = Some(103);
    assert!(perf.enable().is_err());
    assert!(perf.sys().enabled.is_empty());
}

#[test]
fn errors_of_opening_mean_what_the_design_says() {
    let table = [
        (libc::EACCES, false, Status::NotPermitted),
        (libc::EPERM, true, Status::NotPermitted),
        (libc::ENOSYS, false, Status::NoCounters),
        (libc::ENOENT, false, Status::NoCounters),
        (libc::ENOENT, true, Status::KindNotOpenable),
        (libc::EOPNOTSUPP, false, Status::NoCounters),
        (libc::EINVAL, true, Status::KindNotOpenable),
        (libc::EINVAL, false, Status::SystemError),
        (libc::EMFILE, false, Status::NoDescriptor),
        (libc::ENFILE, true, Status::NoDescriptor),
        (libc::EBUSY, false, Status::NoFreeCounter),
        (libc::EIO, false, Status::SystemError),
    ];
    for (errno, typed, status) in table {
        let e = open_error(errno, typed);
        assert_eq!((e.status, e.os_error), (status, errno), "{errno} {typed}");
    }
}

#[test]
fn reads_of_a_group_are_checked() {
    use pmu_counters::counters::perf::parse_group_read;
    let words = |w: &[u64]| w.iter().flat_map(|x| x.to_ne_bytes()).collect::<Vec<u8>>();
    assert_eq!(parse_group_read(&words(&[2, 7, 9])), Some((7, 9)));
    assert_eq!(parse_group_read(&words(&[1, 7, 9])), None);
    assert_eq!(parse_group_read(&words(&[2, 7])), None);
    assert_eq!(parse_group_read(&words(&[2, 7, 9, 0])), None);
    assert_eq!(parse_group_read(&[]), None);
}

#[test]
fn no_free_counter_at_opening_and_a_lost_one_later() {
    let mut perf = Perf::with(FakeSys {
        error_state: true,
        ..FakeSys::default()
    });
    perf.set_plan(plan_of(1, false));
    assert_eq!(perf.open().unwrap_err().status, Status::NoFreeCounter);
    assert!(perf.sys().open.is_empty());

    perf.sys_mut().error_state = false;
    perf.open().unwrap();
    perf.enable().unwrap();
    perf.read().unwrap();
    perf.sys_mut().error_state = true;
    assert_eq!(perf.read().unwrap_err().status, Status::CounterLost);
}

#[test]
fn groups_are_added() {
    let mut perf = Perf::with(FakeSys::default());
    perf.set_plan(plan_of(2, true));
    perf.open().unwrap();
    perf.enable().unwrap();
    let c = perf.read().unwrap();
    // The fake reads 1000 + fd and 2000 + fd for leaders 101 and 103.
    assert_eq!(c.instructions, 1101 + 1103);
    assert_eq!(c.cycles, 2101 + 2103);
}

// ---------------------------------------------------------------------
// The cells
// ---------------------------------------------------------------------

use pmu_counters::cells::{self, Cell, Contents, EMPTYING, G_MASK, LOW_MASK};

/// Which generation an operation uses: the last one taken, one taken
/// before, the last with one of its 44 bits flipped, or any number.
#[derive(Clone, Debug)]
enum Gen {
    Last,
    Old(usize),
    Flip(u32),
    Any(u64),
}

#[derive(Clone, Debug)]
enum CellOp {
    Take,
    Enter(Gen),
    Add(u32, u32),
    Fail(u32),
    Leave,
    Read(Gen),
    Close(Gen),
    Empty,
    Open,
    GiveBack(Gen),
}

fn gen_sel() -> impl Strategy<Value = Gen> {
    prop_oneof![
        3 => Just(Gen::Last),
        1 => any::<usize>().prop_map(Gen::Old),
        1 => (0u32..44).prop_map(Gen::Flip),
        1 => (0..=G_MASK).prop_map(Gen::Any),
    ]
}

fn cell_op() -> impl Strategy<Value = CellOp> {
    prop_oneof![
        Just(CellOp::Take),
        gen_sel().prop_map(CellOp::Enter),
        (any::<u32>(), any::<u32>()).prop_map(|(i, c)| CellOp::Add(i, c)),
        any::<u32>().prop_map(CellOp::Fail),
        Just(CellOp::Leave),
        gen_sel().prop_map(CellOp::Read),
        gen_sel().prop_map(CellOp::Close),
        Just(CellOp::Empty),
        Just(CellOp::Open),
        gen_sel().prop_map(CellOp::GiveBack),
    ]
}

/// The first state of a cell: free at an even generation, or taken at
/// an odd one with some writers, near 0, near the wrap of the
/// generation or anywhere, and near the bound of the writers.
fn first_state() -> impl Strategy<Value = (u64, u64)> {
    let g = prop_oneof![0..8u64, (G_MASK - 8)..=G_MASK, 0..=G_MASK];
    let w = prop_oneof![Just(0u64), 1..4u64, (EMPTYING - 3)..EMPTYING];
    (g, w).prop_map(|(g, w)| if g % 2 == 0 { (g, 0) } else { (g, w) })
}

/// A cell, written plainly.
struct PlainCell {
    g: u64,
    w: u64,
    emptying: bool,
    sums: Contents,
}

impl PlainCell {
    fn taken(&self) -> bool {
        self.g % 2 == 1
    }

    fn take(&mut self) -> Option<u64> {
        if self.taken() || self.emptying {
            return None;
        }
        self.g = (self.g + 1) % (G_MASK + 1);
        Some(self.g)
    }

    fn enter(&mut self, g: u64) -> Result<(), Status> {
        if !self.taken() || self.g != g {
            return Err(Status::StaleToken);
        }
        if self.w + 1 >= EMPTYING {
            return Err(Status::Internal);
        }
        self.w += 1;
        Ok(())
    }

    fn leave(&mut self) -> Result<(), Status> {
        if !self.taken() || self.w == 0 {
            return Err(Status::Internal);
        }
        self.w -= 1;
        Ok(())
    }

    fn read(&self, g: u64) -> Result<Contents, Status> {
        if !self.taken() || self.g != g {
            return Err(Status::StaleToken);
        }
        if self.w != 0 {
            return Err(Status::CellBusy);
        }
        Ok(self.sums)
    }

    fn close(&mut self, g: u64) -> Result<(), Status> {
        self.read(g)?;
        self.g = (self.g + 1) % (G_MASK + 1);
        self.emptying = true;
        Ok(())
    }

    fn empty(&mut self) -> Result<Contents, Status> {
        if !self.emptying {
            return Err(Status::Internal);
        }
        Ok(std::mem::take(&mut self.sums))
    }

    fn open(&mut self) -> Result<(), Status> {
        if !self.emptying {
            return Err(Status::Internal);
        }
        self.emptying = false;
        Ok(())
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn a_cell_follows_the_model(first in first_state(),
                                ops in prop::collection::vec(cell_op(), 0..48)) {
        let cell = Cell::default();
        cell.state.store((first.0 << 20) | first.1, std::sync::atomic::Ordering::SeqCst);
        let mut m = PlainCell { g: first.0, w: first.1, emptying: false,
                                sums: Contents::default() };
        let mut taken = vec![first.0];
        for op in ops {
            let pick = |sel: &Gen| match *sel {
                Gen::Last => taken.last().copied().unwrap_or(0),
                Gen::Old(i) => taken[i % taken.len()],
                Gen::Flip(bit) => taken.last().copied().unwrap_or(0) ^ (1 << bit),
                Gen::Any(g) => g,
            };
            match op {
                CellOp::Take => {
                    let got = cell.take();
                    prop_assert_eq!(got, m.take());
                    taken.extend(got);
                }
                CellOp::Enter(sel) => {
                    let g = pick(&sel);
                    prop_assert_eq!(cell.enter(g), m.enter(g));
                }
                // A worker adds only after it entered.
                CellOp::Add(i, c) if m.taken() && m.w > 0 => {
                    cell.add(u64::from(i), u64::from(c));
                    m.sums.instructions = m.sums.instructions.wrapping_add(u64::from(i));
                    m.sums.cycles = m.sums.cycles.wrapping_add(u64::from(c));
                }
                CellOp::Fail(code) if m.taken() && m.w > 0 => {
                    cell.fail(code);
                    m.sums.failures += 1;
                    m.sums.sqlstate = code;
                }
                CellOp::Add(..) | CellOp::Fail(_) => {}
                CellOp::Leave => prop_assert_eq!(cell.leave(), m.leave()),
                CellOp::Read(sel) => {
                    let g = pick(&sel);
                    prop_assert_eq!(cell.read(g), m.read(g));
                }
                CellOp::Close(sel) => {
                    let g = pick(&sel);
                    prop_assert_eq!(cell.close(g), m.close(g));
                }
                CellOp::Empty => prop_assert_eq!(cell.empty(), m.empty()),
                CellOp::Open => prop_assert_eq!(cell.open(), m.open()),
                CellOp::GiveBack(sel) => {
                    let g = pick(&sel);
                    let want = m.close(g).and_then(|()| m.empty())
                        .and_then(|sums| m.open().map(|()| sums));
                    prop_assert_eq!(cell.give_back(g), want);
                }
            }
            let (g, w) = cell.state();
            let want_w = if m.emptying { EMPTYING } else { m.w };
            prop_assert_eq!((g, w), (m.g, want_w));
        }
    }

    #[test]
    fn tokens_are_valid_exactly_as_the_rule_says(token in prop_oneof![
            any::<u64>(),
            (0u32..300, 0..=G_MASK).prop_map(|(i, g)| cells::token(i, g)),
            (0..=G_MASK).prop_map(|g| g << 20),
            (0..=G_MASK).prop_map(|g| (g << 20) | LOW_MASK),
        ],
        count in prop_oneof![Just(0u32), 1u32..300, Just(LOW_MASK as u32)]) {
        let number = token & LOW_MASK;
        let g = token >> 20;
        let valid = number >= 1 && number <= u64::from(count) && g % 2 == 1;
        let got = cells::split(token, count);
        prop_assert_eq!(got.is_some(), valid);
        if let Some((index, g)) = got {
            prop_assert_eq!(cells::token(index as u32, g), token);
        }
    }

    #[test]
    fn tokens_read_back_from_decimal(token in any::<u64>()) {
        prop_assert_eq!(cells::parse_token(token.to_string().as_bytes()), Ok(token));
    }

    #[test]
    fn decimal_forms_are_refused_as_the_rule_says(
        text in prop_oneof!["[0-9+ a-]{0,24}", "[0-9]{19,24}", "[1-9][0-9]{19}"]) {
        let want = if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
            text.parse::<u64>().map_err(|_| Malformed)
        } else {
            Err(Malformed)
        };
        prop_assert_eq!(cells::parse_token(text.as_bytes()), want);
    }
}

#[test]
fn a_cell_of_the_pool_is_taken_once() {
    let pool = [Cell::default(), Cell::default(), Cell::default()];
    let (a, ga) = cells::take_from_pool(&pool).unwrap();
    let (b, _) = cells::take_from_pool(&pool).unwrap();
    assert_ne!(a, b);
    pool[a].give_back(ga).unwrap();
    assert_eq!(cells::take_from_pool(&pool).map(|(i, _)| i), Some(a));
    let (c, _) = cells::take_from_pool(&pool).unwrap();
    assert!(c != a && c != b);
    assert_eq!(cells::take_from_pool(&pool), None);
}
