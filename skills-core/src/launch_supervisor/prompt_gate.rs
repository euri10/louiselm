//! Holds controller ACP input before the first prompt until sender enrollment.
//!
//! For a contract whose sending runtime is a descendant (louiselm-fkdv8), no
//! prompt may reach the Agent before the Sender guard has enrolled that
//! runtime: prompts start tools, and a tool existing before enrollment could
//! steer the runtime's later authority. Before the hold opens, only complete
//! newline-delimited JSON-RPC lines naming a setup method pass. Anything else,
//! including unparseable or oversized input, stops forwarding and requests
//! enrollment. Holding never grants authority, so every doubt fails closed.

#[cfg(test)]
#[path = "prompt_gate_tests.rs"]
mod tests;

use std::{
    io::{self, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Methods that set up a Session without starting Agent work.
const SETUP_METHODS: [&str; 3] = ["initialize", "authenticate", "session/new"];
/// Bound on one held line, and on bytes read ahead while the hold is closed.
pub(super) const MAX_HELD_LINE: usize = 1024 * 1024;

/// Shared state between the relay and the enrollment owner.
#[derive(Debug, Default)]
pub(super) struct PromptHold {
    opened: AtomicBool,
}

impl PromptHold {
    /// Releases held input. Only the enrollment owner calls this, after success.
    pub(super) fn open(&self) {
        self.opened.store(true, Ordering::Release);
    }

    fn is_open(&self) -> bool {
        self.opened.load(Ordering::Acquire)
    }
}

/// How much of the pending controller input may pass before enrollment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Admission {
    /// Bytes forming complete setup lines, safe to forward now.
    pub(super) forward: usize,
    /// Whether the next input needs an enrolled sender.
    pub(super) hold: bool,
}

/// Classifies pending bytes; pure so every boundary case is unit-testable.
pub(super) fn admit(pending: &[u8]) -> Admission {
    let mut start = 0;
    while let Some(end) = pending[start..].iter().position(|&byte| byte == b'\n') {
        let line = &pending[start..=start + end];
        if !is_setup(line) {
            return Admission {
                forward: start,
                hold: true,
            };
        }
        start += end + 1;
    }
    Admission {
        forward: start,
        hold: pending.len() - start > MAX_HELD_LINE,
    }
}

fn is_setup(line: &[u8]) -> bool {
    if line.iter().all(u8::is_ascii_whitespace) {
        return true;
    }
    serde_json::from_slice::<serde_json::Value>(line).is_ok_and(|message| {
        message
            .get("method")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|method| SETUP_METHODS.contains(&method))
    })
}

/// Controller input staged ahead of the Agent-bound copy buffer.
pub(super) struct GatedInput {
    hold: Arc<PromptHold>,
    pending: Vec<u8>,
    held: bool,
    announced: bool,
    pub(super) eof: bool,
}

impl GatedInput {
    pub(super) fn new(hold: Arc<PromptHold>) -> Self {
        Self {
            hold,
            pending: Vec::new(),
            held: false,
            announced: false,
            eof: false,
        }
    }

    /// Reads more controller input unless held; holding applies backpressure.
    pub(super) fn read(&mut self, input: &mut impl Read) -> io::Result<bool> {
        let open = self.hold.is_open();
        if self.eof || (!open && (self.held || self.pending.len() > MAX_HELD_LINE)) {
            return Ok(false);
        }
        if open && !self.pending.is_empty() {
            return Ok(false);
        }
        let mut buffer = [0; 8 * 1024];
        match input.read(&mut buffer) {
            Ok(0) => {
                self.eof = true;
                Ok(true)
            }
            Ok(read) => {
                self.pending.extend_from_slice(&buffer[..read]);
                Ok(true)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Moves admissible bytes into `output` once it has drained.
    ///
    /// Returns whether any bytes moved or the hold state changed.
    pub(super) fn admit_into(&mut self, output: &mut Vec<u8>) -> bool {
        if !output.is_empty() || self.pending.is_empty() {
            return false;
        }
        if self.hold.is_open() {
            output.append(&mut self.pending);
            self.held = false;
            return true;
        }
        let admission = admit(&self.pending);
        output.extend(self.pending.drain(..admission.forward));
        let newly_held = admission.hold && !self.held;
        self.held |= newly_held;
        admission.forward > 0 || newly_held
    }

    /// True exactly once, when input first becomes held for enrollment.
    pub(super) fn take_request(&mut self) -> bool {
        let first = self.held && !self.announced;
        self.announced |= first;
        first
    }

    /// True when controller EOF has been seen and nothing remains staged.
    pub(super) fn drained(&self) -> bool {
        self.eof && self.pending.is_empty()
    }
}
