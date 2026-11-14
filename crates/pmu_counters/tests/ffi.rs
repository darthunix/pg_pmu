//! The functions C calls for the cells, over memory of the tests.

#![cfg(not(loom))]
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use pmu_counters::Status;
use pmu_counters::cells::{Contents, LOW_MASK, split};
use pmu_counters::ffi::{
    pmu_cell_give_back, pmu_cell_read, pmu_cell_report, pmu_cell_report_failure, pmu_cell_take,
    pmu_cell_take_pool, pmu_cells_init, pmu_cells_size, pmu_token_parse,
};

const OK: i32 = Status::Ok as i32;

/// Memory for `n` cells of the backends and `k` of the pool, set up.
fn memory(n: u32, k: u32) -> Vec<u64> {
    let size = pmu_cells_size(n, k);
    assert_eq!(size, 16 + 32 * (n + k) as usize);
    let mut mem = vec![u64::MAX; size / 8];
    assert_eq!(unsafe { pmu_cells_init(mem.as_mut_ptr().cast(), n, k) }, OK);
    mem
}

fn give_back(base: *const u8, token: u64) -> Result<Contents, i32> {
    let mut out = Contents::default();
    match unsafe { pmu_cell_give_back(base, token, &mut out) } {
        OK => Ok(out),
        e => Err(e),
    }
}

#[test]
fn cells_are_taken_and_given_back() {
    let mem = memory(3, 2);
    let base = mem.as_ptr().cast::<u8>();
    let mut token = 0;

    // A cell of a backend: its own number, once.
    assert_eq!(unsafe { pmu_cell_take(base, 1, &mut token) }, OK);
    assert_eq!(split(token, 5), Some((1, 1)));
    let mine = token;
    assert_eq!(
        unsafe { pmu_cell_take(base, 1, &mut token) },
        Status::Internal as i32
    );
    assert_eq!(
        unsafe { pmu_cell_take(base, 3, &mut token) },
        Status::Internal as i32
    );

    // The pool, after the cells of the backends, until none is free.
    assert_eq!(unsafe { pmu_cell_take_pool(base, &mut token) }, OK);
    assert_eq!(split(token, 5).map(|(i, _)| i), Some(3));
    let pooled = token;
    assert_eq!(unsafe { pmu_cell_take_pool(base, &mut token) }, OK);
    assert_eq!(split(token, 5).map(|(i, _)| i), Some(4));
    assert_eq!(
        unsafe { pmu_cell_take_pool(base, &mut token) },
        Status::NoCell as i32
    );

    // Given back once, then the token is old; a cell given back is free.
    assert_eq!(give_back(base, pooled), Ok(Contents::default()));
    assert_eq!(give_back(base, pooled), Err(Status::StaleToken as i32));
    assert_eq!(unsafe { pmu_cell_take_pool(base, &mut token) }, OK);
    assert_eq!(split(token, 5), Some((3, 3)));
    assert_eq!(give_back(base, mine), Ok(Contents::default()));

    // Tokens that are no tokens of this memory.
    assert_eq!(give_back(base, 0), Err(Status::BadToken as i32));
    assert_eq!(give_back(base, (1 << 20) | 6), Err(Status::BadToken as i32));
    assert_eq!(give_back(base, (2 << 20) | 1), Err(Status::BadToken as i32));
}

#[test]
fn numbers_that_do_not_fit_are_refused() {
    assert_eq!(pmu_cells_size(LOW_MASK as u32, 0), 0);
    assert_eq!(pmu_cells_size(0, LOW_MASK as u32), 0);
    assert_eq!(pmu_cells_size(u32::MAX, u32::MAX), 0);
    assert_ne!(pmu_cells_size(LOW_MASK as u32 - 1, 0), 0);
    let mut mem = vec![0u64; 8];
    let base = mem.as_mut_ptr().cast::<u8>();
    assert_eq!(
        unsafe { pmu_cells_init(base, LOW_MASK as u32, 0) },
        Status::Internal as i32
    );
    assert_eq!(
        unsafe { pmu_cells_init(base.wrapping_add(4), 1, 0) },
        Status::Internal as i32
    );
    assert_eq!(
        unsafe { pmu_cells_init(std::ptr::null_mut(), 1, 0) },
        Status::Internal as i32
    );
}

#[test]
fn null_pointers_are_refused() {
    let mem = memory(1, 1);
    let base = mem.as_ptr().cast::<u8>();
    let mut token = 0;
    let null = std::ptr::null::<u8>();
    assert_eq!(
        unsafe { pmu_cell_take(null, 0, &mut token) },
        Status::Internal as i32
    );
    assert_eq!(
        unsafe { pmu_cell_take_pool(null, &mut token) },
        Status::Internal as i32
    );
    assert_eq!(
        unsafe { pmu_cell_take(base, 0, std::ptr::null_mut()) },
        Status::Internal as i32
    );
    assert_eq!(
        unsafe { pmu_cell_take_pool(base, std::ptr::null_mut()) },
        Status::Internal as i32
    );
    // Nothing was taken by the calls without a place for the token.
    assert_eq!(unsafe { pmu_cell_take(base, 0, &mut token) }, OK);
    assert_eq!(unsafe { pmu_cell_take_pool(base, &mut token) }, OK);
    assert_eq!(
        unsafe { pmu_cell_give_back(base, token, std::ptr::null_mut()) },
        Status::Internal as i32
    );
    assert!(give_back(base, token).is_ok());
}

fn read(base: *const u8, token: u64) -> Result<Contents, i32> {
    let mut out = Contents::default();
    match unsafe { pmu_cell_read(base, token, &mut out) } {
        OK => Ok(out),
        e => Err(e),
    }
}

fn parse(base: *const u8, text: &std::ffi::CStr) -> Result<u64, i32> {
    let mut token = u64::MAX;
    match unsafe { pmu_token_parse(base, text.as_ptr(), &mut token) } {
        OK => Ok(token),
        e => Err(e),
    }
}

#[test]
fn workers_report_into_the_cell_of_their_token() {
    let mem = memory(2, 1);
    let base = mem.as_ptr().cast::<u8>();
    let mut token = 0;
    assert_eq!(unsafe { pmu_cell_take_pool(base, &mut token) }, OK);

    assert_eq!(unsafe { pmu_cell_report(base, token, 10, 20) }, OK);
    assert_eq!(unsafe { pmu_cell_report(base, token, 1, 2) }, OK);
    assert_eq!(unsafe { pmu_cell_report_failure(base, token, 53) }, OK);
    let want = Contents {
        instructions: 11,
        cycles: 22,
        failures: 1,
        sqlstate: 53,
    };
    assert_eq!(read(base, token), Ok(want));
    assert_eq!(give_back(base, token), Ok(want));

    // After the return the token is old, and nothing is added.
    assert_eq!(
        unsafe { pmu_cell_report(base, token, 5, 5) },
        Status::StaleToken as i32
    );
    assert_eq!(
        unsafe { pmu_cell_report_failure(base, token, 53) },
        Status::StaleToken as i32
    );
    assert_eq!(read(base, token), Err(Status::StaleToken as i32));
    assert_eq!(unsafe { pmu_cell_take_pool(base, &mut token) }, OK);
    assert_eq!(read(base, token), Ok(Contents::default()));
    // Tokens that are not valid: no cell, a cell beyond the memory, an
    // even generation; nothing is added anywhere.
    for bad in [0, (1 << 20) | 9, (2 << 20) | 1] {
        assert_eq!(
            unsafe { pmu_cell_report(base, bad, 5, 5) },
            Status::BadToken as i32
        );
        assert_eq!(
            unsafe { pmu_cell_report_failure(base, bad, 53) },
            Status::BadToken as i32
        );
    }
    assert_eq!(read(base, token), Ok(Contents::default()));
}

#[test]
fn tokens_are_read_from_the_setting() {
    let mem = memory(2, 1);
    let base = mem.as_ptr().cast::<u8>();
    let mut token = 0;
    assert_eq!(unsafe { pmu_cell_take(base, 1, &mut token) }, OK);
    let text = std::ffi::CString::new(token.to_string()).unwrap();
    assert_eq!(parse(base, &text), Ok(token));
    assert_eq!(parse(base, c"0"), Ok(0));
    for bad in [c"", c"-1", c" 1", c"x", c"18446744073709551616"] {
        assert_eq!(parse(base, bad), Err(Status::BadToken as i32));
    }
    // Valid in form, but not a cell of this memory.
    let beyond = std::ffi::CString::new(((1u64 << 20) | 4).to_string()).unwrap();
    assert_eq!(parse(base, &beyond), Err(Status::BadToken as i32));
    let even = std::ffi::CString::new(((2u64 << 20) | 1).to_string()).unwrap();
    assert_eq!(parse(base, &even), Err(Status::BadToken as i32));
}
