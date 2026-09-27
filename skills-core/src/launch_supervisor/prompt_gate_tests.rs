//! Pure admission boundaries and the staged reader, without an Agent.
#![allow(
    clippy::unwrap_used,
    reason = "Test fixtures assert setup and observable admission outcomes."
)]

use super::*;

fn line(method: &str) -> Vec<u8> {
    format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"{method}\",\"params\":{{}}}}\n")
        .into_bytes()
}

#[test]
fn only_complete_setup_lines_pass_before_enrollment() {
    let setup = [line("initialize"), line("session/new")].concat();
    assert_eq!(
        admit(&setup),
        Admission {
            forward: setup.len(),
            hold: false
        }
    );
    let prompt = [setup.clone(), line("session/prompt"), line("initialize")].concat();
    assert_eq!(
        admit(&prompt),
        Admission {
            forward: setup.len(),
            hold: true
        }
    );
    let partial = [setup.clone(), b"{\"method\":\"session/new\"".to_vec()].concat();
    assert_eq!(
        admit(&partial),
        Admission {
            forward: setup.len(),
            hold: false
        },
        "an incomplete line waits for its newline"
    );
}

#[test]
fn doubtful_input_holds_instead_of_passing() {
    for doubtful in [
        b"not json\n".to_vec(),
        b"{\"id\":1,\"result\":{}}\n".to_vec(),
        b"{\"method\":7}\n".to_vec(),
        line("session/load"),
        line("session/set_mode"),
        line("Session/New"),
    ] {
        assert_eq!(
            admit(&doubtful),
            Admission {
                forward: 0,
                hold: true
            },
            "{}",
            String::from_utf8_lossy(&doubtful)
        );
    }
    let oversized = vec![b' '; MAX_HELD_LINE + 1];
    assert!(
        admit(&oversized).hold,
        "an unterminated oversized line holds"
    );
    assert_eq!(
        admit(b"\n  \n"),
        Admission {
            forward: 4,
            hold: false
        }
    );
}

#[test]
fn held_input_requests_enrollment_and_flushes_only_after_opening() {
    let hold = Arc::new(PromptHold::default());
    let mut gate = GatedInput::new(Arc::clone(&hold));
    let input = [line("initialize"), line("session/prompt"), line("later")].concat();
    assert!(gate.read(&mut input.as_slice()).unwrap());
    let mut output = Vec::new();
    assert!(gate.admit_into(&mut output));
    assert_eq!(output, line("initialize"));
    assert!(gate.take_request(), "the held prompt requests enrollment");
    assert!(!gate.take_request(), "enrollment is requested once");
    output.clear();
    assert!(!gate.admit_into(&mut output), "held input stays staged");
    assert!(output.is_empty());
    assert!(
        !gate.read(&mut b"more".as_slice()).unwrap(),
        "no read-ahead while held"
    );
    hold.open();
    assert!(gate.admit_into(&mut output));
    assert_eq!(output, [line("session/prompt"), line("later")].concat());
}
