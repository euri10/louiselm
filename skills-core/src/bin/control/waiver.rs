//! Explicit authenticated conformance decisions. Rationale enters through stdin.

use louiselm_skills::broker::{
    operator,
    waiver::{Request, WaiverError},
};
use std::{
    ffi::OsString,
    io::{self, Read, Write},
    path::Path,
};

pub(super) fn cli(arguments: &[OsString]) -> u8 {
    let result = execute(arguments);
    let (bytes, code) = match result {
        Ok(outcome) => match serde_json::to_vec(&outcome) {
            Ok(bytes) => (bytes, 0),
            Err(_) => return 3,
        },
        Err(error) => {
            let payload = serde_json::json!({"schema":"louiselm.conformance-waiver-error/1", "error":error, "next_action":error.next_action()});
            match serde_json::to_vec(&payload) {
                Ok(bytes) => (bytes, 2),
                Err(_) => return 3,
            }
        }
    };
    let written = if code == 0 {
        io::stdout().lock().write_all(&bytes)
    } else {
        io::stderr().lock().write_all(&bytes)
    };
    if written.is_ok() { code } else { 3 }
}

fn execute(
    arguments: &[OsString],
) -> Result<louiselm_skills::broker::waiver::Outcome, WaiverError> {
    let args: Vec<&str> = arguments
        .iter()
        .map(|arg| arg.to_str().ok_or(WaiverError::InvalidRequest))
        .collect::<Result<_, _>>()?;
    let (id, request) = match args.as_slice() {
        ["inspect", id, "--json"] => (*id, Request::Inspect),
        ["plan", id, "--json"] => {
            let mut bytes = Vec::new();
            io::stdin()
                .lock()
                .take(4097)
                .read_to_end(&mut bytes)
                .map_err(|_| WaiverError::InvalidRequest)?;
            if bytes.len() > 4096 {
                return Err(WaiverError::InvalidRequest);
            }
            let proposal =
                serde_json::from_slice(&bytes).map_err(|_| WaiverError::InvalidRequest)?;
            (*id, Request::Plan { proposal })
        }
        ["apply", id, digest, "--json"] => (
            *id,
            Request::Apply {
                plan_digest: (*digest).into(),
            },
        ),
        ["result", id, digest, "--json"] => (
            *id,
            Request::Result {
                plan_digest: (*digest).into(),
            },
        ),
        ["revoke", id, digest, "--json"] => (
            *id,
            Request::Revoke {
                receipt_digest: (*digest).into(),
            },
        ),
        _ => return Err(WaiverError::InvalidRequest),
    };
    operator::validate_subject(id).map_err(|_| WaiverError::InvalidRequest)?;
    let paths = super::installed_paths().map_err(|_| WaiverError::Unavailable)?;
    let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
        .map_err(|_| WaiverError::Unavailable)?;
    operator::conformance_waiver(
        Path::new(operator::SOCKET),
        config.broker_uid,
        id,
        &request,
        operator::TIMEOUT,
    )
    .map_err(|error| match error {
        operator::InspectError::AuthenticationRefused => WaiverError::WrongOperator,
        operator::InspectError::InvalidRequest => WaiverError::InvalidRequest,
        operator::InspectError::UnknownSession => WaiverError::Unknown,
        operator::InspectError::BrokerUnavailable | operator::InspectError::StatusUnavailable => {
            WaiverError::Unavailable
        }
    })?
}
