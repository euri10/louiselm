//! Read-only canonical Beads operation inspection via the authenticated broker.

use louiselm_skills::broker::operator::{self, InspectError};
use std::{
    ffi::OsString,
    io::{self, Write},
    path::Path,
};

pub(super) fn cli(arguments: &[OsString]) -> u8 {
    let result = (|| {
        let [verb, operation_id, format] = arguments else {
            return Err(InspectError::InvalidRequest);
        };
        if verb != "inspect" || format != "--json" {
            return Err(InspectError::InvalidRequest);
        }
        let operation_id = operation_id.to_str().ok_or(InspectError::InvalidRequest)?;
        let paths = super::installed_paths().map_err(|_| InspectError::BrokerUnavailable)?;
        let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
            .map_err(|_| InspectError::BrokerUnavailable)?;
        let inspection = operator::beads_mutation(
            Path::new(operator::SOCKET),
            config.broker_uid,
            operation_id,
            None,
            operator::TIMEOUT,
        )?;
        serde_json::to_vec(&inspection).map_err(|_| InspectError::StatusUnavailable)
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
    fn read_only_cli_refuses_non_inspection_forms_before_loading_authority() {
        for arguments in [
            vec![
                "reconcile",
                "12345678-1234-4234-8234-123456789abc",
                "--json",
            ],
            vec![
                "inspect",
                "12345678-1234-4234-8234-123456789abc",
                "--json",
                "apply",
            ],
            vec!["inspect", "12345678-1234-4234-8234-123456789abc"],
        ] {
            let arguments = arguments
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>();
            assert_eq!(cli(&arguments), InspectError::InvalidRequest.exit_code());
        }
    }
}
