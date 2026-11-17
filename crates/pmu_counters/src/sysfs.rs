//! The kinds of cores of the processor, as Linux lists them in sysfs,
//! and the rule that picks the groups of events to open.
//!
//! Each kind of cores has a unit in `bus/event_source/devices` with a
//! file `cpus`, the list of its processors, and a file `type`. The rule:
//!
//! - more than one such unit: a group for each, with its type;
//! - one unit: a group without a type;
//! - none, as on x86 with one kind of cores, where the unit has no file
//!   `cpus`: a group without a type, if the processor says it has one
//!   kind of cores;
//!
//! and the units must cover the processors online,
//! `devices/system/cpu/online`. Nothing here is specific to Linux but
//! the paths, so it is built and tested everywhere.

use crate::Status;

/// The largest number of processors the lists may name.
pub const MAX_CPUS: usize = 8192;
/// The most kinds of cores the module counts on.
pub const MAX_KINDS: usize = 4;

/// A file of sysfs that cannot be read or does not say what it should.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Malformed;

/// A set of processors, as a bitmap of `64 * W` of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuSet<const W: usize = { MAX_CPUS / 64 }> {
    words: [u64; W],
}

impl<const W: usize> Default for CpuSet<W> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const W: usize> CpuSet<W> {
    pub const fn new() -> Self {
        CpuSet { words: [0; W] }
    }

    /// Add the processors from `first` to `last`, both included.
    pub fn insert_range(&mut self, first: u32, last: u32) -> Result<(), Malformed> {
        if first > last {
            return Err(Malformed);
        }
        let (fw, fb) = ((first / 64) as usize, first % 64);
        let (lw, lb) = ((last / 64) as usize, last % 64);
        if lw >= W {
            return Err(Malformed);
        }
        for (w, word) in self
            .words
            .iter_mut()
            .enumerate()
            .take(lw.wrapping_add(1))
            .skip(fw)
        {
            let low = if w == fw { u64::MAX << fb } else { u64::MAX };
            let high = if w == lw {
                u64::MAX >> 63u32.wrapping_sub(lb)
            } else {
                u64::MAX
            };
            *word |= low & high;
        }
        Ok(())
    }

    /// Add every processor of `other`.
    pub fn union(&mut self, other: &Self) {
        for (a, b) in self.words.iter_mut().zip(other.words.iter()) {
            *a |= *b;
        }
    }

    /// Whether every processor of `other` is in this set.
    pub fn covers(&self, other: &Self) -> bool {
        self.words
            .iter()
            .zip(other.words.iter())
            .all(|(a, b)| b & !a == 0)
    }

    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|w| *w == 0)
    }
}

/// Read a list of processors as the kernel writes it: ranges and
/// single numbers separated by commas, such as `0-3,8,10-11`, with a
/// newline at the end; empty for none. Anything else is refused.
pub fn parse_cpu_list<const W: usize>(text: &[u8]) -> Result<CpuSet<W>, Malformed> {
    let text = trim_end(text);
    let mut set = CpuSet::<W>::new();
    if text.is_empty() {
        return Ok(set);
    }
    for item in text.split(|b| *b == b',') {
        let mut ends = item.splitn(2, |b| *b == b'-');
        let first = parse_u32(ends.next().ok_or(Malformed)?)?;
        let last = match ends.next() {
            Some(e) => parse_u32(e)?,
            None => first,
        };
        set.insert_range(first, last)?;
    }
    Ok(set)
}

/// A decimal number of digits only.
pub fn parse_u32(digits: &[u8]) -> Result<u32, Malformed> {
    if digits.is_empty() {
        return Err(Malformed);
    }
    let mut n: u32 = 0;
    for b in digits {
        let d = b.checked_sub(b'0').filter(|d| *d < 10).ok_or(Malformed)?;
        n = n
            .checked_mul(10)
            .ok_or(Malformed)?
            .checked_add(u32::from(d))
            .ok_or(Malformed)?;
    }
    Ok(n)
}

fn trim_end(text: &[u8]) -> &[u8] {
    let end = text
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |i| i.wrapping_add(1));
    text.get(..end).unwrap_or(&[])
}

/// The groups to open: `count` of them, each with the type of its unit
/// when `typed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub typed: bool,
    pub count: usize,
    pub types: [u32; MAX_KINDS],
}

/// The files the rule reads. Paths are relative to the root of sysfs
/// and given without a terminating zero.
pub trait Fs {
    /// Read a whole small file into `buf`; return its length.
    fn read(&self, path: &[u8], buf: &mut [u8]) -> Result<usize, Malformed>;
    /// Call `each` with the name of every entry of a directory but `.`
    /// and `..`.
    fn list(&self, dir: &[u8], each: &mut dyn FnMut(&[u8])) -> Result<(), Malformed>;
}

const DEVICES: &[u8] = b"bus/event_source/devices";
const ONLINE: &[u8] = b"devices/system/cpu/online";

/// A path built in a buffer, without allocating.
struct Path {
    buf: [u8; 192],
    len: usize,
}

impl Path {
    fn new(parts: &[&[u8]]) -> Result<Path, Malformed> {
        let mut p = Path {
            buf: [0; 192],
            len: 0,
        };
        for part in parts {
            let end = p.len.checked_add(part.len()).ok_or(Malformed)?;
            // Byte by byte: copy_from_slice keeps a panic for slices of
            // other lengths wherever the compiler cannot see they agree.
            let room = p.buf.get_mut(p.len..end).ok_or(Malformed)?;
            for (to, from) in room.iter_mut().zip(part.iter()) {
                *to = *from;
            }
            p.len = end;
        }
        Ok(p)
    }

    fn as_bytes(&self) -> &[u8] {
        self.buf.get(..self.len).unwrap_or(&[])
    }
}

/// Read and parse a list of processors.
fn read_cpus<const W: usize>(fs: &dyn Fs, parts: &[&[u8]]) -> Result<CpuSet<W>, Malformed> {
    let path = Path::new(parts)?;
    // A list of 8192 processors written one by one would not fit, but
    // the kernel writes ranges.
    let mut buf = [0u8; 4096];
    let n = fs.read(path.as_bytes(), &mut buf)?;
    parse_cpu_list(buf.get(..n).ok_or(Malformed)?)
}

/// Read the type of a unit.
fn read_type(fs: &dyn Fs, name: &[u8]) -> Result<u32, Malformed> {
    let path = Path::new(&[DEVICES, b"/", name, b"/type"])?;
    let mut buf = [0u8; 16];
    let n = fs.read(path.as_bytes(), &mut buf)?;
    parse_u32(trim_end(buf.get(..n).ok_or(Malformed)?))
}

/// Apply the rule. `uniform` is what the processor says of its cores:
/// one kind (`Some(true)`), several (`Some(false)`), or nothing, as
/// processors other than x86 say.
pub fn discover(fs: &dyn Fs, uniform: Option<bool>) -> Result<Plan, Status> {
    let mut plan = Plan {
        typed: false,
        count: 0,
        types: [0; MAX_KINDS],
    };
    let mut covered = CpuSet::<{ MAX_CPUS / 64 }>::new();
    let mut units = 0usize;
    // A unit whose cpus cannot be read is not a unit of cores; one whose
    // type cannot be read is left out, and the check of coverage below
    // then fails if its processors are online.
    let listed = fs.list(DEVICES, &mut |name| {
        let Ok(cpus) = read_cpus(fs, &[DEVICES, b"/", name, b"/cpus"]) else {
            return;
        };
        let Ok(t) = read_type(fs, name) else {
            return;
        };
        if let Some(slot) = plan.types.get_mut(units) {
            *slot = t;
        }
        units = units.saturating_add(1);
        covered.union(&cpus);
    });
    if units > MAX_KINDS {
        return Err(Status::TooManyKinds);
    }
    if listed.is_err() || units == 0 {
        // No unit names its processors: one kind of cores, if the
        // processor says so.
        return match uniform {
            Some(true) => Ok(Plan { count: 1, ..plan }),
            Some(false) => Err(Status::CoresNotCovered),
            None => Err(Status::NoCounters),
        };
    }
    let online: CpuSet = read_cpus(fs, &[ONLINE]).map_err(|_| Status::CoresNotCovered)?;
    if !covered.covers(&online) {
        return Err(Status::CoresNotCovered);
    }
    plan.typed = units > 1;
    plan.count = units;
    Ok(plan)
}

/// The files of the system, read with libc.
pub struct LibcFs<'a> {
    /// The root of sysfs, `/sys`.
    pub root: &'a [u8],
}

impl LibcFs<'_> {
    /// The path from the root, ending in zero.
    fn full(&self, path: &[u8]) -> Result<Path, Malformed> {
        Path::new(&[self.root, b"/", path, b"\0"])
    }
}

impl Fs for LibcFs<'_> {
    fn read(&self, path: &[u8], buf: &mut [u8]) -> Result<usize, Malformed> {
        let full = self.full(path)?;
        // SAFETY: the path ends in zero.
        let fd = unsafe {
            libc::open(
                full.as_bytes().as_ptr().cast(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(Malformed);
        }
        let mut len = 0usize;
        let result = loop {
            let Some(rest) = buf.get_mut(len..) else {
                break Err(Malformed);
            };
            if rest.is_empty() {
                // The file is larger than the buffer: refuse it rather
                // than read a part.
                break Err(Malformed);
            }
            // SAFETY: rest is writable for its length.
            let n = unsafe { libc::read(fd, rest.as_mut_ptr().cast(), rest.len()) };
            match usize::try_from(n) {
                Ok(0) => break Ok(len),
                Ok(n) => len = len.saturating_add(n),
                Err(_) => break Err(Malformed),
            }
        };
        // SAFETY: fd was opened above.
        unsafe { libc::close(fd) };
        result
    }

    fn list(&self, dir: &[u8], each: &mut dyn FnMut(&[u8])) -> Result<(), Malformed> {
        let full = self.full(dir)?;
        // SAFETY: the path ends in zero.
        let d = unsafe { libc::opendir(full.as_bytes().as_ptr().cast()) };
        if d.is_null() {
            return Err(Malformed);
        }
        loop {
            // SAFETY: d is open; the entry stays valid until the next
            // call of readdir.
            let entry = unsafe { libc::readdir(d) };
            if entry.is_null() {
                break;
            }
            // SAFETY: d_name ends in zero.
            let name = unsafe { core::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                each(name);
            }
        }
        // SAFETY: d was opened above.
        unsafe { libc::closedir(d) };
        Ok(())
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// A number of up to 12 bytes is read without a panic or an
    /// overflow, and accepted only when it is digits and fits.
    #[kani::proof]
    #[kani::unwind(14)]
    fn numbers_never_overflow() {
        let buf: [u8; 12] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= buf.len());
        let digits = len > 0 && buf[..len].iter().all(|b| b.is_ascii_digit());
        match parse_u32(&buf[..len]) {
            Ok(_) => assert!(digits),
            // Nine digits always fit in 32 bits.
            Err(_) => assert!(!digits || len > 9),
        }
    }

    /// The parser of a list of processors never panics, for every input
    /// of up to 8 bytes, into a set of 128 processors; the code is the
    /// same for every size of set, and proptest covers longer lists.
    #[kani::proof]
    #[kani::unwind(10)]
    fn cpu_list_never_panics() {
        let buf: [u8; 8] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= buf.len());
        let _ = parse_cpu_list::<2>(&buf[..len]);
    }

    /// Adding a range never panics, for every pair of numbers, and adds
    /// exactly the processors from the first to the last.
    #[kani::proof]
    #[kani::unwind(3)]
    fn ranges_are_added_exactly() {
        let first: u32 = kani::any();
        let last: u32 = kani::any();
        let probe: u32 = kani::any();
        kani::assume(probe < 128);
        let mut set = CpuSet::<2>::new();
        let ok = set.insert_range(first, last).is_ok();
        assert_eq!(ok, first <= last && last < 128);
        if ok {
            let mut one = CpuSet::<2>::new();
            assert!(one.insert_range(probe, probe).is_ok());
            assert_eq!(set.covers(&one), first <= probe && probe <= last);
        }
    }
}
