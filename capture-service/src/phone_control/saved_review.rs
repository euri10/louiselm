//! Bounded projection from the optional native saved-result owner.

use std::{
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Component, Path},
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    SavedReviewQuery,
    authority::Refusal,
    source::{PhoneSourceError, read_bounded, trusted_executable, valid_commit, valid_digest},
};
use crate::{
    RunProjectBinding,
    phone_projects::{Directory, directory},
};

const CONTROL: &str = "/usr/local/lib/louiselm/current/bin/louiselm-control";
const MAX_REPLY: usize = 65_536;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeReply {
    kind: String,
    page: NativePage,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativePage {
    schema: String,
    result_id: String,
    run_id: String,
    agent_id: String,
    checkout: String,
    destination: Destination,
    base_commit: String,
    revision: String,
    verification_digest: String,
    plan_digest: String,
    verifier_inputs_digest: String,
    result_digest: String,
    commands_passed: u64,
    current_verification: Verification,
    current_provenance: Provenance,
    file: Option<ReviewFile>,
    next: Option<Cursor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Destination {
    device: u64,
    inode: u64,
    uid: u32,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Side {
    Before,
    After,
}

impl Side {
    fn number(self) -> u8 {
        match self {
            Self::Before => 0,
            Self::After => 1,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Change {
    Added,
    Modified,
    Deleted,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Verification {
    Matching,
    Changed,
    Quarantined,
    Unavailable,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Provenance {
    Untainted,
    SessionOutputTainted,
    Unknown,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReviewFile {
    path: String,
    change: Change,
    side: Side,
    offset: u32,
    text: String,
    binary: bool,
    executable: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    file: u32,
    side: Side,
    offset: u32,
}

/// Runs only the fixed installed verification command, with a closed request.
/// The caller must authorize before this call and recheck before delivery.
pub(super) fn read(query: &SavedReviewQuery) -> Result<NativePage, Refusal> {
    let control = trusted_executable(Path::new(CONTROL)).map_err(|error| source_refusal(&error))?;
    let timeout = trusted_executable(Path::new("/usr/bin/timeout"))
        .map_err(|error| source_refusal(&error))?;
    let input = serde_json::to_vec(&json!({
        "kind":"saved_review_page", "result_id":query.result_id, "run_id":query.run_id,
        "revision":query.revision, "cursor":{"file":query.file,
            "side":if query.side == 0 { "before" } else { "after" }, "offset":query.offset},
        "limit":4096
    }))
    .map_err(|_| Refusal::Unavailable)?;
    let mut child = Command::new(timeout)
        .args(["--signal=TERM", "--kill-after=5s", "30s"])
        .arg(control)
        .args(["verification", "--json"])
        .current_dir("/")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Refusal::Unavailable)?;
    let input_result = child
        .stdin
        .take()
        .ok_or(Refusal::Unavailable)
        .and_then(|mut stdin| stdin.write_all(&input).map_err(|_| Refusal::Unavailable));
    let Some(stdout) = child.stdout.take() else {
        child.wait().map_err(|_| Refusal::Unavailable)?;
        return Err(Refusal::Unavailable);
    };
    let Ok(output) = std::thread::Builder::new()
        .name("louiselm-saved-review-output".into())
        .spawn(move || read_bounded(stdout, MAX_REPLY))
    else {
        // Dropping the output pipe cannot expose content; the timed child is reaped.
        child.wait().map_err(|_| Refusal::Unavailable)?;
        return Err(Refusal::Unavailable);
    };
    let status = child.wait();
    let output = output
        .join()
        .map_err(|_| Refusal::Unavailable)?
        .map_err(|_| Refusal::Unavailable)?;
    input_result?;
    if !status.map_err(|_| Refusal::Unavailable)?.success() || output.len() > MAX_REPLY {
        return Err(Refusal::Unavailable);
    }
    decode(query, &output)
}

fn source_refusal(error: &PhoneSourceError) -> Refusal {
    match error {
        PhoneSourceError::Unsupported => Refusal::Unsupported,
        _ => Refusal::Unavailable,
    }
}

pub(super) fn check_destination(page: &NativePage) -> Result<(), Refusal> {
    let path = Path::new(&page.checkout);
    let current = directory(path).map_err(|_| Refusal::Unavailable)?;
    let metadata = fs::metadata(path).map_err(|_| Refusal::Unavailable)?;
    if !current.matches_destination(
        &page.checkout,
        page.destination.device,
        page.destination.inode,
    ) || metadata.dev() != page.destination.device
        || metadata.ino() != page.destination.inode
        || metadata.uid() != page.destination.uid
    {
        return Err(Refusal::Unavailable);
    }
    Ok(())
}

pub(super) fn decode(query: &SavedReviewQuery, bytes: &[u8]) -> Result<NativePage, Refusal> {
    let reply: NativeReply = serde_json::from_slice(bytes).map_err(|_| Refusal::Unavailable)?;
    let page = reply.page;
    let file_valid = page.file.as_ref().is_none_or(|file| {
        !file.path.is_empty()
            && file.path.len() <= 1025
            && !file.path.contains('\0')
            && Path::new(&file.path)
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            && file.text.len() <= 4096
            && !file.text.contains('\0')
            && (!file.binary || file.text.is_empty())
            && file.offset <= 128 * 1024 * 1024
    });
    let next_valid = page.next.as_ref().is_none_or(|next| {
        next.file <= 10_000
            && next.offset <= 128 * 1024 * 1024
            && (next.file, next.side.number(), next.offset) > (query.file, query.side, query.offset)
            && page.file.is_some()
    });
    if reply.kind != "saved_review_page"
        || page.schema != "louiselm.saved-review-page/1"
        || page.result_id != query.result_id
        || page.run_id != query.run_id
        || page.agent_id.is_empty()
        || page.agent_id.len() > 512
        || page.agent_id.contains('\0')
        || !Path::new(&page.checkout).is_absolute()
        || page.checkout.len() > 8192
        || page.checkout.contains('\0')
        || !valid_commit(&page.base_commit)
        || !valid_digest(&page.revision)
        || page.revision == format!("sha256:{}", "00".repeat(32))
        || query
            .revision
            .as_ref()
            .is_some_and(|expected| expected != &page.revision)
        || !valid_digest(&page.verification_digest)
        || !valid_digest(&page.plan_digest)
        || !valid_digest(&page.verifier_inputs_digest)
        || !valid_digest(&page.result_digest)
        || page.commands_passed > 32
        || !file_valid
        || !next_valid
    {
        return Err(Refusal::Unavailable);
    }
    Ok(page)
}

/// Checks the native destination separately from the original-project grant.
pub(super) fn project(
    page: &NativePage,
    query: &SavedReviewQuery,
    binding: &RunProjectBinding,
    checkout: &Directory,
    operation_id: &str,
) -> Result<Value, Refusal> {
    if binding.run_id != query.run_id
        || page.run_id != query.run_id
        || page.result_id != query.result_id
        || query
            .revision
            .as_ref()
            .is_some_and(|revision| revision != &page.revision)
        || !binding.agents.contains(&page.agent_id)
        || !checkout.matches_destination(
            &page.checkout,
            page.destination.device,
            page.destination.inode,
        )
        || page.destination.uid == 0
    {
        return Err(Refusal::Unavailable);
    }
    Ok(json!({
        "v":1,"type":"saved_review","request_id":operation_id,
        "run_id":page.run_id,"result_id":page.result_id,"revision":&page.revision[7..],
        "producer_agent":page.agent_id,"agents":binding.agents,"destination":page.checkout,
        "base_commit":page.base_commit,"verification_digest":page.verification_digest,
        "plan_digest":page.plan_digest,"verifier_inputs_digest":page.verifier_inputs_digest,
        "result_digest":page.result_digest,"commands_passed":page.commands_passed,
        "current_verification":page.current_verification,"current_provenance":page.current_provenance,
        "file":page.file,"next":page.next
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "fixture setup and assertions")]

    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn native_page_requires_exact_result_cursor_and_retained_checkout() {
        let root = tempfile::tempdir().unwrap();
        let checkout = crate::phone_projects::directory(root.path()).unwrap();
        let metadata = root.path().metadata().unwrap();
        let run = "11111111-1111-4111-8111-111111111111";
        let digest = format!("sha256:{}", "ab".repeat(32));
        let query = SavedReviewQuery {
            run_id: run.into(),
            result_id: "result".into(),
            revision: None,
            file: 0,
            side: 0,
            offset: 0,
        };
        let binding = RunProjectBinding {
            run_id: run.into(),
            project_id: "11".repeat(16),
            source_id: "22".repeat(16),
            checkout_id: "33".repeat(16),
            agents: vec!["codex".into()],
        };
        let mut reply = json!({"kind":"saved_review_page","page":{
            "schema":"louiselm.saved-review-page/1","result_id":"result","run_id":run,
            "agent_id":"codex","checkout":checkout.path,
            "destination":{"device":metadata.dev(),"inode":metadata.ino(),"uid":1000},
            "base_commit":"a".repeat(40),"revision":digest,
            "verification_digest":digest,"plan_digest":digest,"verifier_inputs_digest":digest,
            "result_digest":digest,"commands_passed":1,"current_verification":"matching",
            "current_provenance":"unknown",
            "file":{"path":"src/file","change":"modified","side":"before","offset":0,
                "text":"hello","binary":false,"executable":false},
            "next":{"file":0,"side":"before","offset":5}
        }});
        let parse = |value: &Value| decode(&query, &serde_json::to_vec(value).unwrap());
        let page = parse(&reply).unwrap();
        let projected = project(&page, &query, &binding, &checkout, &"44".repeat(16)).unwrap();
        assert_eq!(projected["file"]["text"], "hello");
        assert_eq!(projected["current_provenance"], "unknown");
        assert!(projected.get("destination").is_some());
        reply["page"]["result_id"] = json!("other");
        assert!(parse(&reply).is_err());
        reply["page"]["result_id"] = json!("result");
        reply["page"]["next"]["offset"] = json!(0);
        assert!(parse(&reply).is_err());
        reply["page"]["next"]["offset"] = json!(5);
        reply["page"]["file"]["path"] = json!("../secret");
        assert!(parse(&reply).is_err());
        reply["page"]["file"]["path"] = json!("src/file");
        reply["page"]["destination"]["inode"] = json!(metadata.ino() + 1);
        let wrong_destination = parse(&reply).unwrap();
        assert!(project(&wrong_destination, &query, &binding, &checkout, "operation").is_err());
        reply["page"]["destination"]["inode"] = json!(metadata.ino());
        reply["page"]["agent_id"] = json!("other");
        let foreign_agent = parse(&reply).unwrap();
        assert!(project(&foreign_agent, &query, &binding, &checkout, "operation").is_err());
    }
}
