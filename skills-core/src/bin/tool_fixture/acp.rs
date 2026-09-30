//! Deterministic ACP peer for the installed Lua-controller gate, never a vendor wrapper.

use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, Write},
};

fn send(output: &mut impl Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()
}

pub(super) fn run(input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let mut pending = None;
    let mut write_promoted_file = false;
    for line in input.lines() {
        let message: Value = serde_json::from_str(&line?)?;
        let id = message["id"].clone();
        let result = match message["method"].as_str() {
            Some("initialize") => json!({"protocolVersion":1,"agentCapabilities":{}}),
            Some("session/new") => json!({"sessionId":"fixture-acp"}),
            Some("session/prompt") => {
                write_promoted_file = message["params"]["prompt"][0]["text"] == "promote-fixture";
                let options = if message["params"]["prompt"][0]["text"] == "unapprovable" {
                    json!([{"optionId":"deny","kind":"reject_once"}])
                } else {
                    json!([{"optionId":"allow","kind":"allow_once"},{"optionId":"deny","kind":"reject_once"}])
                };
                pending = Some(id);
                send(
                    &mut output,
                    &json!({
                        "jsonrpc":"2.0","id":"permission","method":"session/request_permission",
                        "params":{"sessionId":"fixture-acp","toolCall":{"kind":"other"},
                    "options":options}
                    }),
                )?;
                continue;
            }
            Some("session/cancel") => {
                if let Some(id) = pending.take() {
                    send(
                        &mut output,
                        &json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"cancelled"}}),
                    )?;
                }
                continue;
            }
            None if id == "permission" => {
                if message["result"]["outcome"]["optionId"] != "allow" {
                    return Err(io::Error::other("fixture permission was not approved"));
                }
                if write_promoted_file {
                    fs::write("accepted.txt", b"accepted Bead output\n")?;
                    write_promoted_file = false;
                }
                let id = pending
                    .take()
                    .ok_or_else(|| io::Error::other("no pending fixture turn"))?;
                send(
                    &mut output,
                    &json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}}),
                )?;
                continue;
            }
            _ => return Err(io::Error::other("unsupported fixture ACP method")),
        };
        send(
            &mut output,
            &json!({"jsonrpc":"2.0","id":id,"result":result}),
        )?;
    }
    Ok(())
}
