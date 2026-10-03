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

// Synthetic ACP shapes shared with tests/session/api_spec.lua, not a claim
// that a vendor Agent initializes offline or advertises these options.
fn route_options(model: &str, effort: &str) -> Value {
    json!([
        {"id":"model","name":"Model","category":"model","type":"select","currentValue":model,
            "options":[{"value":"fixture-big","name":"Big"},{"value":"fixture-small","name":"Small"}]},
        {"id":"reasoning_effort","name":"Effort","category":"thought_level","type":"select","currentValue":effort,
            "options":[{"value":"high","name":"High"},{"value":"low","name":"Low"}]}
    ])
}

fn qualification_prompt(prompt: &str, output: &mut impl Write, id: &Value) -> io::Result<()> {
    if std::env::var_os("QUALIFICATION_AMBIENT_SECRET").is_some() {
        return Err(io::Error::other("fixture inherited ambient authority"));
    }
    if let Some(outside) = prompt.strip_prefix("qualification-fixture-1|") {
        if fs::read(outside).is_ok() {
            return Err(io::Error::other("fixture read outside its source snapshot"));
        }
    } else if prompt == "qualification-fixture-2" {
        fs::write(".trial-marker", b"independent verification fixture\n")?;
    } else if prompt != "qualification-fixture-3" {
        return Err(io::Error::other("unsupported qualification fixture prompt"));
    }
    send(
        output,
        &json!({"jsonrpc":"2.0","method":"session/update","params":{
            "sessionId":"fixture-acp","update":{"sessionUpdate":"agent_message_chunk",
            "content":{"type":"text","text":"OFFLINE_QUALIFICATION_OK [A]"}}
        }}),
    )?;
    send(
        output,
        &json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}}),
    )
}

pub(super) fn run(input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let mut pending = None;
    let mut promoted_file = None;
    let mut qualification = false;
    let (mut model, mut effort) = ("fixture-big".to_owned(), "high".to_owned());
    for line in input.lines() {
        let message: Value = serde_json::from_str(&line?)?;
        let id = message["id"].clone();
        let result = match message["method"].as_str() {
            Some("initialize") => json!({"protocolVersion":1,"agentCapabilities":{}}),
            Some("session/new") => {
                qualification =
                    message["params"]["_meta"]["louiselmFixture"]["qualification"] == true;
                if qualification {
                    json!({"sessionId":"fixture-acp","configOptions":route_options(&model, &effort)})
                } else {
                    json!({"sessionId":"fixture-acp"})
                }
            }
            Some("session/set_config_option") if qualification => {
                let params = &message["params"];
                match (params["configId"].as_str(), params["value"].as_str()) {
                    (Some("model"), Some(value @ ("fixture-big" | "fixture-small"))) => {
                        value.clone_into(&mut model);
                    }
                    (Some("reasoning_effort"), Some(value @ ("high" | "low"))) => {
                        value.clone_into(&mut effort);
                    }
                    _ => return Err(io::Error::other("unsupported qualification route option")),
                }
                json!({"configOptions":route_options(&model, &effort)})
            }
            Some("session/prompt") if qualification => {
                let prompt = message["params"]["prompt"][0]["text"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("missing qualification fixture prompt"))?;
                qualification_prompt(prompt, &mut output, &id)?;
                continue;
            }
            Some("session/prompt") => {
                promoted_file = match message["params"]["prompt"][0]["text"].as_str() {
                    Some("promote-fixture") => Some("accepted.txt"),
                    Some("promote-fixture-1") => Some("accepted-1.txt"),
                    Some("promote-fixture-2") => Some("accepted-2.txt"),
                    Some("promote-fixture-3") => Some("accepted-3.txt"),
                    _ => None,
                };
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
                if let Some(file) = promoted_file.take() {
                    fs::write(file, b"accepted Bead output\n")?;
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
