//! Local bounded review; approving a candidate never starts a download.

use louiselm_skills::broker::operator::{self, InspectError};
use std::{
    ffi::OsString,
    io::{self, Write},
    path::Path,
};

fn parse(arguments: &[OsString]) -> Result<(&str, Option<Vec<String>>), InspectError> {
    let [verb, subject, rest @ ..] = arguments else {
        return Err(InspectError::InvalidRequest);
    };
    let [ids @ .., format] = rest else {
        return Err(InspectError::InvalidRequest);
    };
    let subject = subject.to_str().ok_or(InspectError::InvalidRequest)?;
    operator::validate_subject(subject)?;
    if format != "--json" {
        return Err(InspectError::InvalidRequest);
    }
    let approve = match verb.to_str() {
        Some("inspect") if ids.is_empty() => None,
        Some("approve") if !ids.is_empty() && ids.len() <= 32 => Some(
            ids.iter()
                .map(|id| {
                    let id = id.to_str().ok_or(InspectError::InvalidRequest)?;
                    if !louiselm_skills::Digest::parse(id)
                        .is_ok_and(|digest| digest.to_string() == id)
                    {
                        return Err(InspectError::InvalidRequest);
                    }
                    Ok(id.to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        _ => return Err(InspectError::InvalidRequest),
    };
    Ok((subject, approve))
}

pub(super) fn cli(arguments: &[OsString]) -> u8 {
    let result = (|| {
        let (subject, approve) = parse(arguments)?;
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
