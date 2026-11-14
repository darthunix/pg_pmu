//! The cells of the shared memory where the parallel workers report.
//!
//! Each counting statement holds a cell: the outermost one of a backend
//! its backend's own, a nested one a cell of the pool. The workers of the
//! statement add their counts to it. The layout and the meaning of the
//! states are in the spec; this module is the only code that reads or
//! writes a cell.
//!
//! The state of a cell is one 64-bit word: the generation `G` in bits
//! 20 to 63, odd while the cell is taken, and the number `W` of workers
//! adding at this moment in bits 0 to 19, `EMPTYING` while the cell is
//! emptied. A worker carries a token, the number of its cell and the `G`
//! it was taken with. Every step on the state is one atomic compare and
//! swap, and every atomic operation is sequentially consistent: a
//! statement does a handful of them, so weaker orders would save nothing.

#[cfg(not(loom))]
use core::sync::atomic::{AtomicU32, AtomicU64};
#[cfg(loom)]
use loom::sync::atomic::{AtomicU32, AtomicU64};

use core::sync::atomic::Ordering::SeqCst;

use crate::Status;
use crate::sysfs::Malformed;

/// Bits of the writers, and of the number of a cell in a token.
pub const LOW_BITS: u32 = 20;
pub const LOW_MASK: u64 = (1 << LOW_BITS) - 1;
/// The writers of a cell that is being emptied.
pub const EMPTYING: u64 = LOW_MASK;
/// The generation counts modulo 2^44.
pub const G_MASK: u64 = (1 << (64 - LOW_BITS)) - 1;

/// The header of the memory, written once when the server starts.
#[repr(C)]
#[derive(Debug)]
pub struct Header {
    /// The cells of the backends.
    pub n: u32,
    /// The cells of the pool.
    pub k: u32,
    pub zero: u64,
}

/// One cell.
#[repr(C)]
#[derive(Debug, Default)]
pub struct Cell {
    pub instructions: AtomicU64,
    pub cycles: AtomicU64,
    pub state: AtomicU64,
    pub failures: AtomicU32,
    pub sqlstate: AtomicU32,
}

/// What a cell holds, as read or emptied.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Contents {
    pub instructions: u64,
    pub cycles: u64,
    pub failures: u32,
    pub sqlstate: u32,
}

const fn generation(state: u64) -> u64 {
    state >> LOW_BITS
}

const fn writers(state: u64) -> u64 {
    state & LOW_MASK
}

/// The state of generation `g` modulo 2^44 and writers `w`.
const fn state_of(g: u64, w: u64) -> u64 {
    ((g & G_MASK) << LOW_BITS) | (w & LOW_MASK)
}

/// The token of cell `index` taken with generation `g`.
pub const fn token(index: u32, g: u64) -> u64 {
    ((g & G_MASK) << LOW_BITS) | ((index as u64).wrapping_add(1) & LOW_MASK)
}

/// The cell and the generation of a token, if it is valid for `cells`
/// cells: a number of the cell from 1 to `cells`, and an odd generation.
/// A token of 0 is no cell, and not valid either.
pub fn split(token: u64, cells: u32) -> Option<(usize, u64)> {
    let number = token & LOW_MASK;
    let g = token >> LOW_BITS;
    if number == 0 || number > u64::from(cells) || g & 1 == 0 {
        return None;
    }
    usize::try_from(number.wrapping_sub(1)).ok().map(|i| (i, g))
}

/// Read a token in decimal: digits only, no sign or spaces, below 2^64.
pub fn parse_token(text: &[u8]) -> Result<u64, Malformed> {
    if text.is_empty() {
        return Err(Malformed);
    }
    let mut n: u64 = 0;
    for b in text {
        let d = b.checked_sub(b'0').filter(|d| *d < 10).ok_or(Malformed)?;
        n = n
            .checked_mul(10)
            .ok_or(Malformed)?
            .checked_add(u64::from(d))
            .ok_or(Malformed)?;
    }
    Ok(n)
}

impl Cell {
    /// The generation and the writers, for the tests.
    pub fn state(&self) -> (u64, u64) {
        let s = self.state.load(SeqCst);
        (generation(s), writers(s))
    }

    /// Take a free cell: from an even generation with no writers to the
    /// next, odd, one. Return that generation, or `None` if the cell is
    /// not free.
    pub fn take(&self) -> Option<u64> {
        let mut s = self.state.load(SeqCst);
        loop {
            let g = generation(s);
            if g & 1 != 0 || writers(s) != 0 {
                return None;
            }
            let taken = state_of(g.wrapping_add(1), 0);
            match self.state.compare_exchange(s, taken, SeqCst, SeqCst) {
                Ok(_) => return Some(generation(taken)),
                Err(now) => s = now,
            }
        }
    }

    /// Enter the cell to add to it, while its generation is `g`, the
    /// generation of the token. The check and the entry are one step.
    pub fn enter(&self, g: u64) -> Result<(), Status> {
        let mut s = self.state.load(SeqCst);
        loop {
            if generation(s) != g || g & 1 == 0 {
                return Err(Status::StaleToken);
            }
            // One more must stay below the mark of emptying.
            if writers(s).wrapping_add(1) >= EMPTYING {
                return Err(Status::Internal);
            }
            match self
                .state
                .compare_exchange(s, s.wrapping_add(1), SeqCst, SeqCst)
            {
                Ok(_) => return Ok(()),
                Err(now) => s = now,
            }
        }
    }

    /// Add counts, between entering and leaving.
    pub fn add(&self, instructions: u64, cycles: u64) {
        self.instructions.fetch_add(instructions, SeqCst);
        self.cycles.fetch_add(cycles, SeqCst);
    }

    /// Count a worker that could not count, between entering and leaving.
    pub fn fail(&self, sqlstate: u32) {
        self.failures.fetch_add(1, SeqCst);
        self.sqlstate.store(sqlstate, SeqCst);
    }

    /// Leave the cell after adding.
    pub fn leave(&self) -> Result<(), Status> {
        let mut s = self.state.load(SeqCst);
        loop {
            let w = writers(s);
            if w == 0 || w == EMPTYING {
                return Err(Status::Internal);
            }
            match self
                .state
                .compare_exchange(s, s.wrapping_sub(1), SeqCst, SeqCst)
            {
                Ok(_) => return Ok(()),
                Err(now) => s = now,
            }
        }
    }

    /// Read what the cell holds, taken with generation `g`, while no
    /// worker is adding.
    pub fn read(&self, g: u64) -> Result<Contents, Status> {
        let s = self.state.load(SeqCst);
        if generation(s) != g || g & 1 == 0 {
            return Err(Status::StaleToken);
        }
        if writers(s) != 0 {
            return Err(Status::CellBusy);
        }
        Ok(self.contents())
    }

    fn contents(&self) -> Contents {
        Contents {
            instructions: self.instructions.load(SeqCst),
            cycles: self.cycles.load(SeqCst),
            failures: self.failures.load(SeqCst),
            sqlstate: self.sqlstate.load(SeqCst),
        }
    }

    /// Close the cell taken with generation `g`: from that generation with
    /// no writers to the next, even, one, with the mark of emptying. From
    /// here every old token is refused, and nobody can take the cell.
    pub fn close(&self, g: u64) -> Result<(), Status> {
        let s = self.state.load(SeqCst);
        if generation(s) != g || g & 1 == 0 {
            return Err(Status::StaleToken);
        }
        if writers(s) != 0 {
            return Err(Status::CellBusy);
        }
        self.state
            .compare_exchange(s, state_of(g.wrapping_add(1), EMPTYING), SeqCst, SeqCst)
            .map(|_| ())
            .map_err(|now| {
                if generation(now) == g && writers(now) != 0 {
                    Status::CellBusy
                } else {
                    Status::StaleToken
                }
            })
    }

    /// Take what a closed cell holds, and set it to zero; the state stays.
    pub fn empty(&self) -> Result<Contents, Status> {
        if writers(self.state.load(SeqCst)) != EMPTYING {
            return Err(Status::Internal);
        }
        Ok(Contents {
            instructions: self.instructions.swap(0, SeqCst),
            cycles: self.cycles.swap(0, SeqCst),
            failures: self.failures.swap(0, SeqCst),
            sqlstate: self.sqlstate.swap(0, SeqCst),
        })
    }

    /// Free an emptied cell: the mark of emptying becomes no writers.
    pub fn open(&self) -> Result<(), Status> {
        let s = self.state.load(SeqCst);
        if writers(s) != EMPTYING || generation(s) & 1 != 0 {
            return Err(Status::Internal);
        }
        self.state
            .compare_exchange(s, state_of(generation(s), 0), SeqCst, SeqCst)
            .map(|_| ())
            .map_err(|_| Status::Internal)
    }

    /// Give back the cell taken with generation `g`: close, empty, open.
    /// Return what it held, to hand down.
    pub fn give_back(&self, g: u64) -> Result<Contents, Status> {
        self.close(g)?;
        let contents = self.empty()?;
        self.open()?;
        Ok(contents)
    }
}

/// Bytes of the memory for `n` cells of backends and `k` of the pool.
pub fn size(n: u32, k: u32) -> Option<usize> {
    let cells = usize::try_from(n)
        .ok()?
        .checked_add(usize::try_from(k).ok()?)?;
    cells
        .checked_mul(core::mem::size_of::<Cell>())?
        .checked_add(core::mem::size_of::<Header>())
}

/// Take a free cell of the pool, `pool` being the cells of the pool, and
/// return its place in the pool and its generation.
pub fn take_from_pool(pool: &[Cell]) -> Option<(usize, u64)> {
    pool.iter()
        .enumerate()
        .find_map(|(i, cell)| cell.take().map(|g| (i, g)))
}

#[cfg(all(test, not(loom)))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use core::mem::{offset_of, size_of};

    #[test]
    fn layout_is_that_of_the_spec() {
        assert_eq!(size_of::<Header>(), 16);
        assert_eq!(size_of::<Cell>(), 32);
        assert_eq!(offset_of!(Cell, instructions), 0);
        assert_eq!(offset_of!(Cell, cycles), 8);
        assert_eq!(offset_of!(Cell, state), 16);
        assert_eq!(offset_of!(Cell, failures), 24);
        assert_eq!(offset_of!(Cell, sqlstate), 28);
        assert_eq!(size(136, 16), Some(4880));
    }

    #[test]
    fn a_cell_goes_round() {
        let c = Cell::default();
        let g = c.take().unwrap();
        assert_eq!(g, 1);
        assert!(c.take().is_none());
        c.enter(g).unwrap();
        c.add(10, 20);
        c.leave().unwrap();
        assert_eq!(c.read(g).unwrap().instructions, 10);
        let got = c.give_back(g).unwrap();
        assert_eq!((got.instructions, got.cycles), (10, 20));
        assert_eq!(c.state(), (2, 0));
        assert_eq!(c.enter(g), Err(Status::StaleToken));
        assert_eq!(c.take(), Some(3));
        assert_eq!(c.read(3).unwrap(), Contents::default());
    }

    #[test]
    fn a_cell_with_a_writer_is_not_given_back() {
        let c = Cell::default();
        let g = c.take().unwrap();
        c.enter(g).unwrap();
        assert_eq!(c.give_back(g), Err(Status::CellBusy));
        c.leave().unwrap();
        assert!(c.give_back(g).is_ok());
    }

    #[test]
    fn the_generation_wraps() {
        let c = Cell::default();
        c.state.store(state_of(G_MASK - 1, 0), SeqCst);
        let g = c.take().unwrap();
        assert_eq!(g, G_MASK);
        c.give_back(g).unwrap();
        assert_eq!(c.state(), (0, 0));
        assert_eq!(c.take(), Some(1));
    }

    #[test]
    fn tokens_read_back() {
        let t = token(5, 7);
        assert_eq!(split(t, 10), Some((5, 7)));
        assert_eq!(split(t, 5), None);
        assert_eq!(split(token(0, 2), 10), None);
        assert_eq!(split(0, 10), None);
        assert_eq!(parse_token(b"18446744073709551615"), Ok(u64::MAX));
        assert!(parse_token(b"18446744073709551616").is_err());
        assert!(parse_token(b"+1").is_err());
        assert!(parse_token(b" 1").is_err());
        assert!(parse_token(b"").is_err());
        assert_eq!(parse_token(b"000000000000000000000007"), Ok(7));
    }
}
