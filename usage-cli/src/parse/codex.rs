//! Codex rollout records, including executions nested inside code-mode tools.

use crate::command;
use crate::model::{Call, Evidence, Parsed, Request, Session, output, text, timestamp, usage};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct State {
    native_id: Option<String>,
    project: Option<String>,
    turn: Option<String>,
    model: Option<String>,
    route: Option<String>,
    options: BTreeMap<String, Value>,
}

impl State {
    pub fn consume(&mut self, event: &Value, source: &str, line: u64, parsed: &mut Parsed) {
        let p = &event["payload"];
        match event["type"].as_str() {
            Some("session_meta") => self.session(p, parsed),
            Some("turn_context") => {
                self.turn = text(&p["turn_id"]);
                self.model = text(&p["model"]);
                if p["cwd"].is_string() {
                    self.project = text(&p["cwd"]);
                }
                self.options.clear();
                for key in ["effort", "approval_policy", "summary"] {
                    if p[key].is_string() || p[key].is_boolean() {
                        self.options.insert(format!("codex.{key}"), p[key].clone());
                    }
                }
            }
            Some("response_item") => self.response(p, event, source, line, parsed),
            Some("event_msg") if p["type"] == "token_usage_record" => {
                self.usage(p, event, source, line, parsed);
            }
            Some("event_msg") if p["type"] == "token_count" => {
                // Legacy cumulative snapshots lack a unique response key. Never sum them.
                parsed.note("legacy_usage_snapshot_not_summed");
            }
            Some("event_msg") if p["type"] == "item_completed" => {
                match p["item"]["type"].as_str() {
                    Some("CommandExecution") => {
                        self.execution(&p["item"], event, source, line, parsed);
                    }
                    Some("FileChange" | "Extension") => {
                        self.operation(&p["item"], event, source, line, parsed);
                    }
                    Some(
                        "AgentMessage" | "UserMessage" | "Reasoning" | "ContextCompaction" | "Plan",
                    ) => (),
                    _ => parsed.note("unhandled_structured_item"),
                }
            }
            _ => (),
        }
    }

    fn operation(&self, item: &Value, event: &Value, source: &str, line: u64, parsed: &mut Parsed) {
        let Some(id) = item["id"].as_str() else {
            parsed.note("missing_call_id");
            return;
        };
        let Some(base) = self.call(id, event) else {
            parsed.note("unassociated_call");
            return;
        };
        let call = parsed.calls.entry(base.id.clone()).or_insert(base);
        if item["type"] == "FileChange" {
            call.tool = "<file change>".into();
            output(call, &item["stdout"]);
        } else {
            call.tool = text(&item["kind"]).unwrap_or_else(|| "<extension>".into());
        }
        call.status = text(&item["status"]).unwrap_or_else(|| "result_observed".into());
        call.evidence.push(Evidence {
            source_id: source.into(),
            record: line,
            row_id: None,
        });
    }

    fn usage(&self, p: &Value, event: &Value, source: &str, line: u64, parsed: &mut Parsed) {
        let Some(native) = &self.native_id else {
            parsed.note("unassociated_usage");
            return;
        };
        let Some(response) = text(&p["response_id"]) else {
            parsed.note("usage_without_response_id");
            return;
        };
        let session_id = format!("codex:{native}");
        let request = Request {
            id: format!("{session_id}:response:{response}"),
            session_id,
            adapter: "codex".into(),
            project: self.project.clone(),
            model: self.model.clone(),
            provider_route: self.route.clone(),
            time: timestamp(&event["timestamp"]),
            scope: "response".into(),
            basis: "native_reported".into(),
            usage: usage(
                &p["usage"],
                &[
                    ("/input_tokens", "input_tokens"),
                    ("/output_tokens", "output_tokens"),
                    ("/total_tokens", "total_tokens"),
                    ("/reasoning_output_tokens", "thought_tokens"),
                    ("/cached_input_tokens", "cached_read_tokens"),
                    ("/cache_write_input_tokens", "cached_write_tokens"),
                ],
            ),
            evidence: vec![Evidence {
                source_id: source.into(),
                record: line,
                row_id: None,
            }],
        };
        parsed.requests.insert(request.id.clone(), request);
    }

    fn session(&mut self, p: &Value, parsed: &mut Parsed) {
        let Some(native_id) = text(&p["id"]) else {
            parsed.note("missing_session_id");
            return;
        };
        self.native_id = Some(native_id.clone());
        self.project = text(&p["cwd"]);
        self.route = text(&p["model_provider"]);
        let id = format!("codex:{native_id}");
        parsed.sessions.insert(
            id.clone(),
            Session {
                id,
                native_id,
                adapter: "codex".to_owned(),
                project: self.project.clone(),
                louiselm_origin: p["originator"] == "louiselm.nvim" && p["session_id"] == p["id"],
                ..Session::default()
            },
        );
    }

    fn call(&self, id: &str, event: &Value) -> Option<Call> {
        let native = self.native_id.as_ref()?;
        let session_id = format!("codex:{native}");
        Some(Call {
            id: format!("{session_id}:call:{id}"),
            session_id,
            native_session_id: native.clone(),
            adapter: "codex".to_owned(),
            turn_id: self.turn.clone(),
            project: self.project.clone(),
            model: self.model.clone(),
            provider_route: self.route.clone(),
            options: self.options.clone(),
            time: timestamp(&event["timestamp"]),
            level: "leaf".to_owned(),
            status: "unobserved".to_owned(),
            ..Call::default()
        })
    }

    fn response(&self, p: &Value, event: &Value, source: &str, line: u64, parsed: &mut Parsed) {
        let Some(id) = p["call_id"].as_str() else {
            return;
        };
        let Some(mut base) = self.call(id, event) else {
            parsed.note("unassociated_call");
            return;
        };
        match p["type"].as_str() {
            Some("function_call" | "custom_tool_call") => {
                p["name"]
                    .as_str()
                    .unwrap_or("<unknown>")
                    .clone_into(&mut base.tool);
                "requested".clone_into(&mut base.status);
                if p["type"] == "custom_tool_call" && p["name"] == "exec" {
                    "orchestrator".clone_into(&mut base.level);
                }
                if let Some(args) = p["arguments"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    && let Some(cmd) = args["cmd"].as_str().or_else(|| args["command"].as_str())
                {
                    command::extract(&mut base, cmd);
                }
                base.evidence.push(Evidence {
                    source_id: source.to_owned(),
                    record: line,
                    row_id: None,
                });
                // Replayed request records must not erase an already observed terminal.
                parsed.calls.entry(base.id.clone()).or_insert(base);
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                let call = parsed.calls.entry(base.id.clone()).or_insert(base);
                output(call, &p["output"]);
                if call.status == "requested" || call.status == "unobserved" {
                    "result_observed".clone_into(&mut call.status);
                }
                call.evidence.push(Evidence {
                    source_id: source.to_owned(),
                    record: line,
                    row_id: None,
                });
            }
            _ => (),
        }
    }

    fn execution(&self, item: &Value, event: &Value, source: &str, line: u64, parsed: &mut Parsed) {
        let Some(id) = item["id"].as_str() else {
            parsed.note("missing_execution_id");
            return;
        };
        let Some(base) = self.call(id, event) else {
            parsed.note("unassociated_call");
            return;
        };
        let call = parsed.calls.entry(base.id.clone()).or_insert(base);
        if call.tool.is_empty() {
            "<command execution>".clone_into(&mut call.tool);
        }
        let command = item["command"]
            .as_array()
            .and_then(|argv| {
                if argv.len() == 3 && argv[1].as_str().is_some_and(|s| ["-c", "-lc"].contains(&s)) {
                    argv[2].as_str().map(str::to_owned)
                } else {
                    None
                }
            })
            .or_else(|| text(&item["command"]));
        if let Some(cmd) = command {
            command::extract(call, &cmd);
        }
        if let Some(project) = text(&item["cwd"]) {
            call.project = Some(project);
        }
        call.exit_code = item["exit_code"].as_i64();
        item["status"]
            .as_str()
            .unwrap_or("unobserved")
            .clone_into(&mut call.status);
        call.duration_ms = item["duration"]["secs"].as_f64().map(|seconds| {
            seconds * 1000.0 + item["duration"]["nanos"].as_f64().unwrap_or(0.0) / 1_000_000.0
        });
        call.produced_output_bytes = item["aggregated_output"].as_str().map(|s| s.len() as u64);
        let result = if item["formatted_output"].is_null() {
            &item["aggregated_output"]
        } else {
            &item["formatted_output"]
        };
        output(call, result);
        call.evidence.push(Evidence {
            source_id: source.to_owned(),
            record: line,
            row_id: None,
        });
    }
}
