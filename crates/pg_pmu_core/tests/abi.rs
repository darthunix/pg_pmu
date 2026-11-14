//! The codes of `pg_pmu_core.h` against the crate's.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use pg_pmu_core::Status;

const ALL: [Status; 16] = [
    Status::Ok,
    Status::NotPermitted,
    Status::NoSystem,
    Status::NoCounters,
    Status::CoresNotCovered,
    Status::KindNotOpenable,
    Status::NoFreeCounter,
    Status::NoDescriptor,
    Status::CounterLost,
    Status::SystemError,
    Status::Internal,
    Status::TooManyKinds,
    Status::StaleToken,
    Status::CellBusy,
    Status::NoCell,
    Status::BadToken,
];

/// The C name of each code, as the header spells it.
fn c_name(status: Status) -> &'static str {
    match status {
        Status::Ok => "PMU_OK",
        Status::NotPermitted => "PMU_NOT_PERMITTED",
        Status::NoSystem => "PMU_NO_SYSTEM",
        Status::NoCounters => "PMU_NO_COUNTERS",
        Status::CoresNotCovered => "PMU_CORES_NOT_COVERED",
        Status::KindNotOpenable => "PMU_KIND_NOT_OPENABLE",
        Status::NoFreeCounter => "PMU_NO_FREE_COUNTER",
        Status::NoDescriptor => "PMU_NO_DESCRIPTOR",
        Status::CounterLost => "PMU_COUNTER_LOST",
        Status::SystemError => "PMU_SYSTEM_ERROR",
        Status::Internal => "PMU_INTERNAL",
        Status::TooManyKinds => "PMU_TOO_MANY_KINDS",
        Status::StaleToken => "PMU_STALE_TOKEN",
        Status::CellBusy => "PMU_CELL_BUSY",
        Status::NoCell => "PMU_NO_CELL",
        Status::BadToken => "PMU_BAD_TOKEN",
    }
}

#[test]
fn header_codes_match_the_crate() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pg_pmu_core.h");
    let header = std::fs::read_to_string(path).expect("the header");
    let defines: Vec<(String, i32)> = header
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            if words.next()? != "#define" {
                return None;
            }
            let name = words.next()?;
            let value = words.next()?.parse().ok()?;
            Some((name.to_string(), value))
        })
        .collect();
    assert_eq!(defines.len(), ALL.len(), "codes in the header: {defines:?}");
    for status in ALL {
        let name = c_name(status);
        let value = defines
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("{name} is not in the header"))
            .1;
        assert_eq!(value, status as i32, "{name}");
    }
}
