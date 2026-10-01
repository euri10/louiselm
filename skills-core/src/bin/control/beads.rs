//! Read-only canonical Beads operation inspection via the authenticated broker.

use louiselm_skills::broker::operator::{self, InspectError};
use std::{
    io::{self, Write},
    path::Path,
};

pub(super) fn cli(arguments: Result<&clap::ArgMatches, ()>) -> u8 {
    let result = (|| {
        let (_, args) =
            super::arguments::operation(arguments).map_err(|()| InspectError::InvalidRequest)?;
        let operation_id = args
            .get_one::<String>("subject")
            .ok_or(InspectError::InvalidRequest)?;
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
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            let matches = super::super::arguments::command().try_get_matches_from(
                [
                    std::ffi::OsString::from("louiselm-control"),
                    std::ffi::OsString::from("beads"),
                ]
                .into_iter()
                .chain(arguments),
            );
            assert_eq!(
                cli(matches
                    .as_ref()
                    .ok()
                    .and_then(|m| m.subcommand().map(|(_, m)| m))
                    .ok_or(())),
                InspectError::InvalidRequest.exit_code()
            );
        }
    }
}
