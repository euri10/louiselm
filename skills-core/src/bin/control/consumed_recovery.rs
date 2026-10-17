//! Existing unprivileged daemon's separate root-only recovery listener.
use louiselm_skills::{
    broker::{
        BrokerError, InstalledBroker,
        operator::{
            InspectError, OperatorServer,
            consumed_recovery::{Request, Response, SOCKET},
        },
    },
    launch_transport::TransportError,
};
use std::{io, path::Path, sync::Arc, thread};

pub(super) fn start(broker: Arc<InstalledBroker>) -> Result<(), BrokerError> {
    let endpoint = OperatorServer::bind(Path::new(SOCKET), 0).map_err(BrokerError::Storage)?;
    thread::Builder::new()
        .name("louiselm-consumed-recovery".into())
        .spawn(move || {
            loop {
                let result = endpoint.serve_recovery_once(|request| {
                    let result = match request {
                        Request::Prepare { authorization_id } => broker
                            .consumed_recovery_snapshot(&authorization_id)
                            .map(|snapshot| Response::Snapshot {
                                snapshot: Box::new(snapshot),
                            }),
                        Request::Commit { proof } => broker
                            .accept_consumed_recovery(&proof)
                            .map(|acknowledgement| Response::Acknowledged { acknowledgement }),
                    };
                    result.map_err(|error| match error {
                        BrokerError::UnknownAuthorization => InspectError::UnknownSession,
                        BrokerError::InvalidGrant
                        | BrokerError::RequestMismatch
                        | BrokerError::ReceiptUnauthorized
                        | BrokerError::ReceiptRefused(_)
                        | BrokerError::Recovery(_) => InspectError::InvalidRequest,
                        _ => InspectError::StatusUnavailable,
                    })
                });
                if let Err(error) = result {
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    eprintln!("louiselm-control: consumed recovery listener unavailable");
                    break;
                }
            }
        })
        .map(drop)
        .map_err(|_| BrokerError::Transport(TransportError::WorkerUnavailable))
}
