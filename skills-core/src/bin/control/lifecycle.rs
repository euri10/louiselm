//! Installed operator lifecycle CLI. Stdin supplies CAS fields, never caller authority.

use louiselm_skills::{
    broker::operator::{self, InspectError},
    launch_protocol::{LifecycleRequest, MAX_PROTOCOL_MESSAGE_BYTES},
};
use std::{
    io::{self, Read, Write},
    path::Path,
};

fn read_request(input: impl Read) -> Result<LifecycleRequest, InspectError> {
    let mut bytes = Vec::new();
    input
        .take((MAX_PROTOCOL_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| InspectError::InvalidRequest)?;
    if bytes.is_empty() || bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
        return Err(InspectError::InvalidRequest);
    }
    let request = serde_json::from_slice(&bytes).map_err(|_| InspectError::InvalidRequest)?;
    operator::validate_lifecycle_request(&request)?;
    Ok(request)
}

pub(super) fn cli(arguments: Result<&clap::ArgMatches, ()>) -> u8 {
    let result: Result<_, InspectError> = (|| {
        arguments.map_err(|()| InspectError::InvalidRequest)?;
        let request = read_request(io::stdin())?;
        let paths = super::installed_paths().map_err(|_| InspectError::BrokerUnavailable)?;
        let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
            .map_err(|_| InspectError::BrokerUnavailable)?;
        let response = operator::lifecycle(
            Path::new(operator::SOCKET),
            config.broker_uid,
            &request,
            operator::TIMEOUT,
        )?;
        let refused = response.is_err();
        let bytes = serde_json::to_vec(&response).map_err(|_| InspectError::StatusUnavailable)?;
        Ok((bytes, refused))
    })();
    match result {
        Ok((bytes, refused)) => {
            if io::stdout().lock().write_all(&bytes).is_err() {
                return InspectError::StatusUnavailable.exit_code();
            }
            if refused { 7 } else { 0 }
        }
        Err(error) => {
            // A closed diagnostic sink cannot change the already refused operation.
            let _ = io::stderr().lock().write_all(&error.canonical_bytes());
            error.exit_code()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "Closed input fixtures assert parsing outcomes."
    )]
    use super::*;

    #[test]
    fn input_is_bounded_and_cannot_supply_caller_authority() {
        let request = serde_json::json!({
            "schema": louiselm_skills::launch_protocol::LIFECYCLE_REQUEST_SCHEMA,
            "protocol_version": louiselm_skills::launch::PROTOCOL_VERSION,
            "request_id": "request", "session_id": "session", "run_id": "run",
            "authorization_id": "authorization", "action": "park", "expected_state": "running",
            "expected_receipt_sequence": 1, "envelope_revision": 1
        });
        let valid = serde_json::to_vec(&request).unwrap();
        assert_eq!(
            read_request(valid.as_slice()).unwrap().canonical_bytes(),
            serde_json::from_slice::<LifecycleRequest>(&valid)
                .unwrap()
                .canonical_bytes()
        );
        for field in [
            "caller",
            "operator_uid",
            "uid",
            "role",
            "deadline",
            "expires_at_ms",
        ] {
            let mut foreign = request.clone();
            foreign[field] = serde_json::json!(0);
            assert_eq!(
                read_request(serde_json::to_vec(&foreign).unwrap().as_slice()),
                Err(InspectError::InvalidRequest)
            );
        }
        for bytes in [
            vec![],
            vec![b' '; MAX_PROTOCOL_MESSAGE_BYTES + 1],
            b"{}".to_vec(),
            b"{".to_vec(),
        ] {
            assert_eq!(
                read_request(bytes.as_slice()),
                Err(InspectError::InvalidRequest)
            );
        }
    }
}
