//! Installed operator CLI for broker-owned source/cache staging.

use louiselm_skills::broker::operator::{self, InspectError, LaunchInputsRequest};
use std::{
    io::{self, Read, Write},
    path::Path,
};

fn parse(bytes: &[u8]) -> Result<LaunchInputsRequest, InspectError> {
    if bytes.is_empty()
        || bytes.len() > louiselm_skills::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES
    {
        return Err(InspectError::InvalidRequest);
    }
    LaunchInputsRequest::parse(bytes)
}

pub(super) fn cli(arguments: Result<&clap::ArgMatches, ()>) -> u8 {
    let result = (|| {
        arguments.map_err(|()| InspectError::InvalidRequest)?;
        let mut bytes = Vec::new();
        io::stdin()
            .take((louiselm_skills::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| InspectError::InvalidRequest)?;
        let request = parse(&bytes)?;
        let paths = super::installed_paths().map_err(|_| InspectError::BrokerUnavailable)?;
        let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
            .map_err(|_| InspectError::BrokerUnavailable)?;
        let response = operator::stage_launch_inputs(
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
    use std::ffi::OsString;
    fn parse(args: &[OsString], bytes: &[u8]) -> Result<(), InspectError> {
        super::super::arguments::command()
            .try_get_matches_from(
                [
                    OsString::from("louiselm-control"),
                    OsString::from("launch-inputs"),
                ]
                .into_iter()
                .chain(args.iter().cloned()),
            )
            .map_err(|_| InspectError::InvalidRequest)?;
        super::parse(bytes).map(|_| ())
    }

    #[test]
    fn staging_cli_refuses_invalid_input_unbounded_input_and_extra_arguments() {
        let args = [OsString::from("stage"), OsString::from("--json")];
        assert!(parse(&args, br#"{"manifest":{},"snapshot":"/snapshot","cache":"/cache","expected_base_commit":"a","command":"forbidden"}"#).is_err());
        assert!(
            parse(
                &args,
                &vec![b'x'; louiselm_skills::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES + 1]
            )
            .is_err()
        );
        assert!(parse(&[OsString::from("stage")], b"{}").is_err());
    }
}
