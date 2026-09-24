//! Explicit cross-crate gate: real producer output through capture into Neovim.
use super::*;
use louiselm_skills::broker::attention::{AttentionEndpoint, Outbox, ProjectionChange};
use std::collections::BTreeMap;

pub(crate) struct Observer {
    endpoint: AttentionEndpoint,
    expected: BTreeMap<String, serde_json::Value>,
}

impl Observer {
    pub(crate) fn new() -> Self {
        Self {
            endpoint: serde_json::from_slice(
                &fs::read(std::env::var_os("LOUISELM_REQUEST_ENDPOINT").unwrap()).unwrap(),
            )
            .unwrap(),
            expected: BTreeMap::new(),
        }
    }

    pub(crate) fn drain(&mut self, outbox: &Outbox) -> Vec<ProjectionChange> {
        let mut changes = vec![];
        while let Some(item) = outbox.next().unwrap() {
            let wire = item.wire();
            match &item.change {
                ProjectionChange::Upsert(condition) => {
                    self.expected.insert(
                        condition.operation_id.clone(),
                        wire["change"]["attention"].clone(),
                    );
                }
                ProjectionChange::Clear(condition) => {
                    self.expected.remove(&condition.operation_id);
                }
                ProjectionChange::ClearSubject(subject) => {
                    use louiselm_skills::broker::attention::AttentionSubject;
                    let (kind, id) = match subject {
                        AttentionSubject::Session(id) => ("session", id),
                        AttentionSubject::Run(id) => ("run", id),
                    };
                    self.expected.retain(|_, draft| {
                        draft["subject_kind"] != kind || draft["subject_id"] != *id
                    });
                }
            }
            assert!(
                outbox
                    .deliver_next(&self.endpoint)
                    .unwrap_or_else(|error| panic!(
                        "delivery code {}: {error}",
                        wire["change"]["attention"]["code"]
                    ))
            );
            changes.push(item.change);
        }
        // Every observation is a fresh editor: state must survive absent/restarted Neovim.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let output = std::process::Command::new("nvim")
            .args(["--headless", "--noplugin", "-u", "NONE", "-l"])
            .arg(root.join("tests/fixtures/attention_observer.lua"))
            .env(
                "LOUISELM_ATTENTION_EXPECTED",
                serde_json::to_string(&self.expected.values().collect::<Vec<_>>()).unwrap(),
            )
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Neovim observer: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        changes
    }
}
