//! Installed operator entrypoint for exact Run verification operations.

use louiselm_skills::broker::operator::{self, InspectError, VerificationControlRequest};
use std::{
    io::{self, Read, Write},
    path::Path,
};

pub(super) fn cli(arguments: Result<&clap::ArgMatches, ()>) -> u8 {
    let result = (|| {
        arguments.map_err(|()| InspectError::InvalidRequest)?;
        let mut bytes = Vec::new();
        io::stdin()
            .take((louiselm_skills::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| InspectError::InvalidRequest)?;
        if bytes.is_empty()
            || bytes.len() > louiselm_skills::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES
        {
            return Err(InspectError::InvalidRequest);
        }
        let request: VerificationControlRequest =
            serde_json::from_slice(&bytes).map_err(|_| InspectError::InvalidRequest)?;
        let paths = super::installed_paths().map_err(|_| InspectError::BrokerUnavailable)?;
        let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
            .map_err(|_| InspectError::BrokerUnavailable)?;
        let response = operator::verification(
            Path::new(operator::SOCKET),
            config.broker_uid,
            &request,
            operator::VERIFICATION_TIMEOUT,
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
