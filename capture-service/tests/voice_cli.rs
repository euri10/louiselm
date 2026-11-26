//! Real capture startup with intentionally unavailable transcription.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Isolated fixtures assert setup and outcomes."
)]

use std::{
    fmt::Write as _,
    io::Read,
    net::{SocketAddr, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use louiselm_capture::{PairingRegistry, Store, TlsIdentity};
use sha2::{Digest, Sha256};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
#[ignore = "requires a disposable private network namespace"]
fn missing_transcription_configuration_preserves_real_capture_upload() {
    for invalid_model in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let data = root.path().join("data");
        let config = root.path().join("config");
        let tls = TlsIdentity::load_or_create(state.join("louiselm/capture/tls")).unwrap();
        let pairing = PairingRegistry::open(state.join("louiselm/capture/pairing")).unwrap();
        let offer = pairing
            .issue(
                "https://localhost:7391",
                tls.public_key_sha256(),
                1000,
                10_000,
            )
            .unwrap();
        let phone = pairing.consume(&offer.token, "fixture", 1001).unwrap();
        drop(pairing);
        let mut command = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LOUISELM_CAPTURE_STATE_DIR", &state)
            .env("LOUISELM_CAPTURE_DATA_DIR", &data)
            .env("LOUISELM_CAPTURE_CONFIG_DIR", config)
            .env("LOUISELM_RECEIVER_ENABLED", "true")
            .env("LOUISELM_TRANSCRIPTION_ENABLED", "true")
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if invalid_model {
            command.env("LOUISELM_CAPTURE_OPENAI_API_KEY", "fixture-key");
            command.env("LOUISELM_TRANSCRIPTION_MODEL", "   ");
        }
        let mut child = ChildGuard(command.spawn().unwrap());
        let address: SocketAddr = "127.0.0.1:7391".parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_err() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "capture stopped for invalid transcription"
            );
            assert!(Instant::now() < deadline, "fixture receiver did not start");
            thread::sleep(Duration::from_millis(10));
        }
        let certificate =
            reqwest::Certificate::from_pem(&std::fs::read(tls.certificate_path()).unwrap())
                .unwrap();
        let client = reqwest::blocking::Client::builder()
            .add_root_certificate(certificate)
            .build()
            .unwrap();
        let audio = b"ordinary dummy audio";
        let mut digest = String::with_capacity(64);
        for byte in Sha256::digest(audio) {
            write!(&mut digest, "{byte:02x}").unwrap();
        }
        let id = "11111111-1111-4111-8111-111111111111";
        let response = client
            .put(format!("https://localhost:7391/v1/captures/{id}"))
            .bearer_auth(phone.credential)
            .header("content-type", "audio/mp4")
            .header("x-louiselm-source", "android")
            .header("x-louiselm-recorded-at-ms", "100")
            .header("x-louiselm-duration-ms", "50")
            .header("x-louiselm-sha256", digest)
            .body(audio.as_slice())
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CREATED);
        let capture = Store::new(data.join("louiselm/captures"))
            .unwrap()
            .capture(id)
            .unwrap();
        assert_eq!(std::fs::read(capture.audio_path).unwrap(), audio);
        assert!(!data.join("louiselm/voice-captures").exists());
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let mut stderr = String::new();
        child
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(stderr.contains("transcription unavailable"));
    }
}
