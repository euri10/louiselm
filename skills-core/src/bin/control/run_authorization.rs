//! Bounded operator Run authorization through the installed broker socket.

use louiselm_skills::broker::operator::{self, AuthorizationRequest, InspectError};
use std::{
    ffi::OsString,
    io::{self, Read, Write},
    path::Path,
};

fn parse(arguments: &[OsString], bytes: &[u8]) -> Result<AuthorizationRequest, InspectError> {
    if arguments != ["authorize", "--json"]
        || bytes.is_empty()
        || bytes.len() > louiselm_skills::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES
    {
        return Err(InspectError::InvalidRequest);
    }
    serde_json::from_slice(bytes).map_err(|_| InspectError::InvalidRequest)
}

pub(super) fn cli(arguments: &[OsString]) -> u8 {
    let result = (|| {
        let mut bytes = Vec::new();
        io::stdin()
            .take((louiselm_skills::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| InspectError::InvalidRequest)?;
        let request = parse(arguments, &bytes)?;
        let paths = super::installed_paths().map_err(|_| InspectError::BrokerUnavailable)?;
        let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
            .map_err(|_| InspectError::BrokerUnavailable)?;
        let response = operator::authorization(
            Path::new(operator::SOCKET),
            config.broker_uid,
            &request,
            operator::TIMEOUT,
        )?;
        serde_json::to_vec(&response).map_err(|_| InspectError::StatusUnavailable)
    })();
    match result {
        Ok(bytes) => match io::stdout().lock().write_all(&bytes) {
            Ok(()) => 0,
            Err(_) => InspectError::StatusUnavailable.exit_code(),
        },
        Err(error) => {
            let _ = io::stderr().lock().write_all(&error.canonical_bytes());
            error.exit_code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_rejects_unknown_fields_and_unbounded_input_before_installation() {
        let args = [OsString::from("authorize"), OsString::from("--json")];
        assert!(parse(&args, br#"{"kind":"run","envelope":{},"extra":true}"#).is_err());
        assert!(
            parse(
                &args,
                &vec![b'x'; louiselm_skills::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES + 1]
            )
            .is_err()
        );
        assert!(parse(&[OsString::from("approve")], b"{}").is_err());
    }
}
