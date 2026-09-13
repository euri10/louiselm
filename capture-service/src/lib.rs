//! Durable, local-first capture records and filesystem storage.

mod attention;
mod attention_socket;
pub mod cli;
mod fcm;
mod generation;
mod identity;
mod model;
mod network;
mod openai;
mod operator_socket;
mod pairing;
mod permissions;
mod receiver;
mod run_socket;
mod runs;
mod store;
mod time;
mod transcription;

pub use attention::{
    AttentionCode, AttentionDraft, AttentionError, AttentionItem, AttentionKey, AttentionKind,
    AttentionSnapshot, AttentionStore, AttentionSubjectKind, AttentionSummary, BrokerProjection,
    ProjectionChange, ProjectionResult,
};
pub use attention_socket::{AttentionSocket, AttentionSocketError, AttentionSocketMessage};
pub use generation::{
    BeadsGenerator, CommandOutput, GenerateRequest, GeneratedIssue, GenerationError,
    mutation_external_ref,
};
pub use identity::{IdentityError, TlsIdentity};
pub use model::{
    CaptureDraft, CaptureRecord, CaptureSource, CaptureState, Transcript, TranscriptionState,
};
pub use network::{NetworkProfile, NetworkProfileError, NetworkProfileKind};
pub use openai::OpenAiTranscriber;
pub use pairing::{
    DeviceCredential, DeviceStatus, NotificationFailure, NotificationHealth, NotificationStatus,
    NotificationTargetStatus, PairingError, PairingOffer, PairingRegistry, PairingStatus,
};
pub use receiver::Receiver;
pub use run_socket::{RunSocket, RunSocketError, RunSocketMessage};
pub use runs::{
    BeadsCleanup, GeneratedWorkBudget, GeneratedWorkReservation, ReapAction, ReapSummary,
    ReserveResult, ResumeResult, Run, RunAdmission, RunDraft, RunSession, RunStore, RunStoreError,
    RunSummary, RunView,
};
pub use store::{Capture, IngestOutcome, MAX_CAPTURE_BYTES, Store, StoreError};
pub use transcription::{
    Transcriber, TranscriptRequest, TranscriptionError, TranscriptionErrorKind, TranscriptionWorker,
};
