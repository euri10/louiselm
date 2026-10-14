//! Retired guard links must not survive into the next installed fixture.

use std::{
    thread,
    time::{Duration, Instant},
};

pub(super) fn policy_maps() -> Vec<u32> {
    libbpf_rs::query::MapInfoIter::default()
        .filter(|map| map.name.as_bytes() == b"policy")
        .map(|map| map.id)
        .collect()
}

pub(super) fn assert_released(ids: &[u32]) {
    // Detached deadline/transport workers inherit the owner's private bpffs.
    // Their exit releases its pinned programs after Session teardown. Returning
    // earlier lets a retired, deny-only guard match a reused netns inode/port.
    // Probe exact object lifetimes: delay alone must never count as cleanup.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let mut retained = Vec::new();
        for id in ids {
            match libbpf_rs::MapHandle::from_map_id(*id) {
                Ok(_) => retained.push(*id),
                Err(error) if error.kind() == libbpf_rs::ErrorKind::NotFound => (),
                Err(error) => panic!("fixture guard policy map {id} release unproven: {error}"),
            }
        }
        if retained.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "fixture guard maps survived cleanup: {retained:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
