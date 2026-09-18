//! Read-only diagnostic linked to the guest's existing dev-profile skills library.
//! See ledger.md for compilation and workload; this is not a CI gate.

use std::{fs, hint::black_box, time::Instant};

fn main() {
    assert_eq!(
        louiselm_skills::Digest::of(b"abc").hex(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    for path in std::env::args().skip(1) {
        let bytes = fs::read(&path).unwrap();
        let expected = louiselm_skills::Digest::of(&bytes);
        for sample in 1..=5 {
            let start = Instant::now();
            let digest = louiselm_skills::Digest::of(black_box(&bytes));
            let seconds = start.elapsed().as_secs_f64();
            assert_eq!(digest, expected);
            println!(
                "{path} bytes={} sample={sample} seconds={seconds:.6}",
                bytes.len()
            );
        }
    }
}
