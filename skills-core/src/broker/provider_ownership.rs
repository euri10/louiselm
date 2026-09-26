//! Process-local proof that every handed-off Provider descriptor is gone.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

use crate::launch_protocol::GuardScope;

use super::BrokerError;

#[derive(Default)]
struct Revision {
    enrolled: bool,
    closed: bool,
    leases: Vec<Weak<()>>,
}

impl Revision {
    fn has_live_lease(&mut self) -> bool {
        self.leases.retain(|lease| lease.strong_count() != 0);
        !self.leases.is_empty()
    }
}

/// Shared by all concurrent broker workers, including reconnected Sessions.
#[derive(Default)]
pub(super) struct ProviderOwnership {
    revisions: Mutex<BTreeMap<(String, u64), Revision>>,
}

impl ProviderOwnership {
    pub(super) fn listener(&self, scope: &GuardScope, lease: &Arc<()>) -> Result<(), BrokerError> {
        let mut revisions = self
            .revisions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for ((session_id, revision), prior) in revisions.iter_mut() {
            if session_id == &scope.session_id
                && *revision != scope.revision
                && (!prior.closed || prior.has_live_lease())
            {
                return Err(BrokerError::ProviderUnavailable);
            }
        }
        let current = revisions
            .entry((scope.session_id.clone(), scope.revision))
            .or_default();
        if current.enrolled || current.closed {
            return Err(BrokerError::ProviderUnavailable);
        }
        current.enrolled = true;
        current.leases.push(Arc::downgrade(lease));
        Ok(())
    }

    pub(super) fn socket(&self, scope: &GuardScope, lease: &Arc<()>) -> Result<(), BrokerError> {
        let mut revisions = self
            .revisions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = revisions
            .get_mut(&(scope.session_id.clone(), scope.revision))
            .ok_or(BrokerError::ProviderUnavailable)?;
        if !current.enrolled || current.closed {
            return Err(BrokerError::ProviderUnavailable);
        }
        current.leases.push(Arc::downgrade(lease));
        Ok(())
    }

    pub(super) fn close(&self, scope: &GuardScope) -> Result<(), BrokerError> {
        let mut revisions = self
            .revisions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = revisions
            .entry((scope.session_id.clone(), scope.revision))
            .or_default();
        if current.has_live_lease() {
            return Err(BrokerError::ProviderUnavailable);
        }
        current.closed = true;
        Ok(())
    }
}
