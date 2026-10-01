//! Explicit authenticated Provider budget extensions for a held Run.

use louiselm_skills::broker::{
    operator,
    provider_extension::{ExtensionError, ExtensionOutcome, ExtensionRequest},
};
use std::{
    io::{self, Write},
    path::Path,
};

pub(super) fn cli(arguments: Result<&clap::ArgMatches, ()>) -> u8 {
    let (bytes, code) = match execute(arguments) {
        Ok(outcome) => match serde_json::to_vec(&outcome) {
            Ok(bytes) => (bytes, 0),
            Err(_) => return 3,
        },
        Err(error) => {
            let payload = serde_json::json!({"schema":"louiselm.provider-extension-error/1", "error":error, "next_action":error.next_action()});
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

fn execute(arguments: Result<&clap::ArgMatches, ()>) -> Result<ExtensionOutcome, ExtensionError> {
    let args = arguments.map_err(|()| ExtensionError::InvalidRequest)?;
    let id = args
        .get_one::<String>("subject")
        .ok_or(ExtensionError::InvalidRequest)?;
    let request = ExtensionRequest {
        request_id: args
            .get_one::<String>("request_id")
            .ok_or(ExtensionError::InvalidRequest)?
            .clone(),
        additional_requests: *args
            .get_one::<u32>("requests")
            .ok_or(ExtensionError::InvalidRequest)?,
        expires_at_ms: args.get_one::<u64>("expires_at_ms").copied(),
    };
    operator::validate_subject(id).map_err(|_| ExtensionError::InvalidRequest)?;
    let paths = super::installed_paths().map_err(|_| ExtensionError::Unavailable)?;
    let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
        .map_err(|_| ExtensionError::Unavailable)?;
    operator::provider_extension(
        Path::new(operator::SOCKET),
        config.broker_uid,
        id,
        &request,
        operator::TIMEOUT,
    )
    .map_err(|error| match error {
        operator::InspectError::AuthenticationRefused => ExtensionError::WrongOperator,
        operator::InspectError::InvalidRequest => ExtensionError::InvalidRequest,
        operator::InspectError::UnknownSession => ExtensionError::Unknown,
        operator::InspectError::BrokerUnavailable | operator::InspectError::StatusUnavailable => {
            ExtensionError::Unavailable
        }
    })?
}
