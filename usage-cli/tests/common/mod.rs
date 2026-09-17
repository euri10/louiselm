//! Disposable CLI fixtures; no test reads real histories.

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static SERIAL: AtomicU64 = AtomicU64::new(0);

pub struct Fixture(pub PathBuf);

impl Fixture {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "louiselm-usage-test-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    pub fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_louiselm-usage"))
            .arg("--db")
            .arg(self.0.join("state/index.sqlite3"))
            .args(args)
            .output()
            .unwrap()
    }

    pub fn ok(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{} / {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    pub fn log(&self, format: &str, name: &str, events: &[Value]) -> String {
        let path = self.0.join(name);
        let text = events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&path, text).unwrap();
        format!("{format}={}", path.display())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // This exclusively created directory contains only this test's artifacts.
        fs::remove_dir_all(&self.0).unwrap();
    }
}
