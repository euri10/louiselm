//! Durable, local-first capture records and filesystem storage.

pub mod cli;
mod identity;
mod model;
mod openai;
mod pairing;
mod receiver;
mod store;
mod transcription;

pub use identity::{IdentityError, TlsIdentity};
pub use model::{
    CaptureDraft, CaptureRecord, CaptureSource, CaptureState, Transcript, TranscriptionState,
};
pub use openai::OpenAiTranscriber;
pub use pairing::{
    DeviceCredential, DeviceStatus, PairingError, PairingOffer, PairingRegistry, PairingStatus,
};
pub use receiver::Receiver;
pub use store::{Capture, IngestOutcome, MAX_CAPTURE_BYTES, Store, StoreError};
pub use transcription::{
    Transcriber, TranscriptRequest, TranscriptionError, TranscriptionErrorKind, TranscriptionWorker,
};
