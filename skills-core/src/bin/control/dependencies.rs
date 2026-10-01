//! Local bounded review; approving a candidate never starts a download.

use louiselm_skills::broker::operator::{self, InspectError};
use std::{
    io::{self, Write},
    path::Path,
};

fn parse(matches: &clap::ArgMatches) -> Result<(&str, Option<Vec<String>>), InspectError> {
    let (verb, args) = matches.subcommand().ok_or(InspectError::InvalidRequest)?;
    let subject = args
        .get_one::<String>("subject")
        .ok_or(InspectError::InvalidRequest)?
        .as_str();
    operator::validate_subject(subject)?;
    let approve = match verb {
        "inspect" => None,
        "approve" => Some(
            args.get_many::<String>("candidates")
                .ok_or(InspectError::InvalidRequest)?
                .map(|id| {
                    if !louiselm_skills::Digest::parse(id)
                        .is_ok_and(|digest| digest.to_string() == *id)
                    {
                        return Err(InspectError::InvalidRequest);
                    }
                    Ok(id.clone())
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        _ => return Err(InspectError::InvalidRequest),
    };
    Ok((subject, approve))
}

pub(super) fn cli(arguments: Result<&clap::ArgMatches, ()>) -> u8 {
    let result = (|| {
        let (subject, approve) = parse(arguments.map_err(|()| InspectError::InvalidRequest)?)?;
        let paths = super::installed_paths().map_err(|_| InspectError::BrokerUnavailable)?;
        let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
            .map_err(|_| InspectError::BrokerUnavailable)?;
        let view = operator::dependencies(
            Path::new(operator::SOCKET),
            config.broker_uid,
            subject,
            approve,
            operator::TIMEOUT,
        )?;
        serde_json::to_vec(&view).map_err(|_| InspectError::StatusUnavailable)
    })();
    match result {
        Ok(bytes) => match io::stdout().lock().write_all(&bytes) {
            Ok(()) => 0,
            Err(_) => InspectError::StatusUnavailable.exit_code(),
        },
        Err(error) => {
            // Diagnostic failure cannot change the refusal or grant authority.
            let _ = io::stderr().lock().write_all(&error.canonical_bytes());
            error.exit_code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    fn parse(args: &[OsString]) -> Result<(), InspectError> {
        let matches = super::super::arguments::command()
            .try_get_matches_from(
                [
                    OsString::from("louiselm-control"),
                    OsString::from("dependencies"),
                ]
                .into_iter()
                .chain(args.iter().cloned()),
            )
            .map_err(|_| InspectError::InvalidRequest)?;
        super::parse(matches.subcommand().ok_or(InspectError::InvalidRequest)?.1).map(|_| ())
    }
    #[test]
    fn dependency_cli_accepts_only_inspection_or_exact_bounded_approval() {
        let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(parse(&args(&["inspect", "session", "--json"])).is_ok());
        assert!(
            parse(&args(&[
                "approve",
                "session",
                &louiselm_skills::Digest::of(b"candidate").to_string(),
                "--json"
            ]))
            .is_ok()
        );
        for values in [
            &["approve", "session", "--json"][..],
            &["approve", "session", "*", "--json"],
            &["inspect", "../foreign", "--json"],
            &["inspect", "session", "extra", "--json"],
        ] {
            assert!(parse(&args(values)).is_err());
        }
    }
}
