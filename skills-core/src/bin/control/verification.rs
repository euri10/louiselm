//! Installed operator entrypoint for exact Run verification operations.

use louiselm_skills::{
    broker::operator::{
        self, InspectError, VerificationControlRequest, VerificationControlResponse,
    },
    workspace::promotion::DestinationIdentity,
};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

fn review_destination(path: &Path, expected: DestinationIdentity) -> Result<PathBuf, InspectError> {
    let identity =
        DestinationIdentity::inspect(path).map_err(|_| InspectError::StatusUnavailable)?;
    let canonical = path
        .canonicalize()
        .map_err(|_| InspectError::StatusUnavailable)?;
    let canonical_identity =
        DestinationIdentity::inspect(&canonical).map_err(|_| InspectError::StatusUnavailable)?;
    if identity != expected || canonical_identity != expected {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(canonical)
}

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
        let mut response = operator::verification(
            Path::new(operator::SOCKET),
            config.broker_uid,
            &request,
            operator::VERIFICATION_TIMEOUT,
        )?;
        if let VerificationControlResponse::SavedReviewPage { page } = &mut response {
            page.checkout = review_destination(&page.checkout, page.destination)?;
        }
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
    #![allow(
        clippy::unwrap_used,
        reason = "A failed fixture or identity check must fail the test."
    )]

    use super::*;
    use louiselm_skills::workspace::promotion::DestinationIdentity;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn saved_review_refuses_a_replaced_destination() {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        std::fs::set_permissions(&checkout, std::fs::Permissions::from_mode(0o700)).unwrap();
        let expected = DestinationIdentity::inspect(&checkout).unwrap();
        assert_eq!(
            review_destination(&checkout, expected).unwrap(),
            checkout.canonicalize().unwrap()
        );
        std::fs::rename(&checkout, root.path().join("original")).unwrap();
        std::fs::create_dir(&checkout).unwrap();
        std::fs::set_permissions(&checkout, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(review_destination(&checkout, expected).is_err());
    }
}
