//! Process-owned outbox delivery, independent of Session workers.

use louiselm_skills::broker::{BrokerError, InstalledBroker, attention::AttentionEndpoint};
use std::{
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

const POLL: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(30);

pub(super) fn start(broker: Arc<InstalledBroker>) -> Result<JoinHandle<()>, BrokerError> {
    thread::Builder::new()
        .name("louiselm-broker-attention".into())
        .spawn(move || {
            let mut retry = POLL;
            let mut unavailable = false;
            // This worker and its bounded transport worker belong to the daemon
            // process. Native SIGTERM closes both immediately, even mid-read.
            // Only deliver_attention can persist an exact receiver ACK.
            loop {
                let endpoint = AttentionEndpoint::installed();
                // Local terminal cleanup still runs when delivery is unconfigured.
                let result = broker.deliver_attention(endpoint.as_ref().ok());
                match result {
                    Ok(delivered) => {
                        if unavailable && delivered {
                            eprintln!("louiselm-control: Attention delivery available");
                            unavailable = false;
                        }
                        retry = POLL;
                        if !delivered {
                            thread::sleep(POLL);
                        }
                    }
                    Err(error) => {
                        if !unavailable {
                            // Do not render transport/parser errors or endpoint
                            // contents: peer input may contain sensitive data.
                            eprintln!("louiselm-control: Attention delivery unavailable ({error}); queued entries retained; check endpoint configuration, receiver and outbox storage");
                            unavailable = true;
                        }
                        thread::sleep(retry);
                        retry = (retry * 2).min(MAX_RETRY);
                    }
                }
            }
        })
        .map_err(BrokerError::Attention)
}
