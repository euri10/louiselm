//! Explicit authenticated conformance decisions. Rationale enters through stdin.

use louiselm_skills::broker::{
    operator,
    waiver::{Request, WaiverError},
};
use std::{
    io::{self, Read, Write},
    path::Path,
};

pub(super) fn cli(arguments: Result<&clap::ArgMatches, ()>) -> u8 {
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
    arguments: Result<&clap::ArgMatches, ()>,
) -> Result<louiselm_skills::broker::waiver::Outcome, WaiverError> {
    let (verb, args) =
        super::arguments::operation(arguments).map_err(|()| WaiverError::InvalidRequest)?;
    let id = args
        .get_one::<String>("subject")
        .ok_or(WaiverError::InvalidRequest)?;
    let request = match verb {
        "inspect" => Request::Inspect,
        "plan" => {
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
            Request::Plan { proposal }
        }
        "apply" => Request::Apply {
            plan_digest: args
                .get_one::<String>("digest")
                .ok_or(WaiverError::InvalidRequest)?
                .clone(),
        },
        "result" => Request::Result {
            plan_digest: args
                .get_one::<String>("digest")
                .ok_or(WaiverError::InvalidRequest)?
                .clone(),
        },
        "revoke" => Request::Revoke {
            receipt_digest: args
                .get_one::<String>("digest")
                .ok_or(WaiverError::InvalidRequest)?
                .clone(),
        },
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
