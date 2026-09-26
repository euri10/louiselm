//! Exact descriptor acceptance and socket-before-lease closure acknowledgements.
use super::{GuardError, SenderGuard, namespace_id};
use crate::launch_protocol::{
    GuardEnrollment, GuardScope, PROTOCOL_VERSION, ProtocolResponse, RESPONSE_SCHEMA,
    ResponseResult,
};
use crate::launch_transport::LauncherPacket;
use std::sync::mpsc;
use std::{net::SocketAddr, os::fd::AsFd, time::Duration};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HandoffStage {
    Connected,
    Queued,
    AckWait,
}

impl SenderGuard {
    /// Connects one already-admitted broker destination and transfers the guarded
    /// socket and both leases on the original authenticated channel. No TLS or
    /// Provider bytes are sent here. The caller serializes protocol receives.
    /// # Errors
    /// Refuses lost/stale scope, connection failure or an inexact acknowledgement.
    /// Failed handoff shuts down the socket; uncertain cleanup prevents reuse.
    pub fn handoff_upstream(
        &mut self,
        scope: &GuardScope,
        destination: SocketAddr,
        request_id: &str,
        timeout: Duration,
    ) -> Result<u64, GuardError> {
        self.handoff_upstream_with(scope, destination, request_id, timeout, |_| {})
    }

    pub(super) fn handoff_upstream_with(
        &mut self,
        scope: &GuardScope,
        destination: SocketAddr,
        request_id: &str,
        timeout: Duration,
        progress: impl Fn(HandoffStage),
    ) -> Result<u64, GuardError> {
        let enrollment = self
            .handoff
            .clone()
            .filter(|_| self.announced)
            .ok_or(GuardError::Enrollment)?;
        let socket = self.connect_upstream_with(scope, destination, timeout, || {
            progress(HandoffStage::Connected);
        })?;
        let cookie = socket.cookie()?;
        let evidence = crate::launch_protocol::GuardUpstream {
            enrollment,
            destination,
            socket_cookie: cookie,
            network_id: namespace_id(&socket.network)?,
        };
        let response = response(
            request_id,
            ResponseResult::SenderGuardUpstream {
                socket: evidence.clone(),
            },
        );
        response.validate().map_err(|_| GuardError::Authority)?;
        let (sent, completion) = mpsc::sync_channel(1);
        let result = self
            .broker
            .send_descriptors(
                response.canonical_bytes(),
                socket.descriptors(),
                Box::new(move |value| {
                    let _ = sent.send(value);
                }),
            )
            .map_err(|_| GuardError::Lost)
            .and_then(|()| {
                progress(HandoffStage::Queued);
                completion
                    .recv_timeout(timeout)
                    .map_err(|_| GuardError::Lost)?
                    .map_err(|_| GuardError::Lost)
            })
            .and_then(|()| {
                progress(HandoffStage::AckWait);
                self.receive_ack(
                    request_id,
                    &ResponseResult::SenderGuardUpstreamAccepted { socket: evidence },
                    timeout,
                )
            })
            .and_then(|()| {
                let state = self.revocable()?;
                if state.revoked || state.revision != scope.revision {
                    Err(GuardError::Authority)
                } else {
                    Ok(())
                }
            });
        if result.is_err() {
            self.broker.close();
            // Channel loss is not a kernel policy transition. Revoke the
            // endpoint and every upstream while both processes may still live.
            self.revoke()?;
        }
        result?;
        Ok(cookie)
    }

    /// Transfers the endpoint and namespace leases over the authenticated channel.
    /// Only an exact broker acknowledgement permits subsequent activation.
    /// The caller must exclusively own protocol receive ordering for this exchange.
    /// # Errors
    /// Refuses incomplete/repeated enrollment, invalid evidence or lost delivery.
    /// Uncertain transfer remains owned until acknowledged closure or broker death.
    pub fn announce_enrollment(
        &mut self,
        request_id: &str,
        timeout: Duration,
    ) -> Result<(), GuardError> {
        self.check_scope(&self.scope)?;
        if !self.enrolled || self.handoff.is_some() {
            return Err(GuardError::Enrollment);
        }
        let endpoint = self.endpoint.as_ref().ok_or(GuardError::Enrollment)?;
        let pins = self.pins.lease()?;
        let enrollment = GuardEnrollment {
            scope: self.scope.clone(),
            guard_id: self.pins.id()?,
            runtime_pid: self.runtime_pid,
            broker_pid: self.broker.peer_credentials().pid,
            address: endpoint
                .listener
                .local_addr()
                .map_err(|_| GuardError::Socket)?,
            listener_cookie: endpoint.rule.listener,
            network_id: endpoint.rule.namespace,
        };
        let response = response(
            request_id,
            ResponseResult::SenderGuardEnrolled {
                enrollment: enrollment.clone(),
            },
        );
        response.validate().map_err(|_| GuardError::Authority)?;
        self.handoff = Some(enrollment.clone());
        let (sent, completion) = mpsc::sync_channel(1);
        let result = self
            .broker
            .send_descriptors(
                response.canonical_bytes(),
                [
                    endpoint.listener.as_fd(),
                    pins.as_fd(),
                    endpoint.network.as_fd(),
                ],
                Box::new(move |result| {
                    let _ = sent.send(result);
                }),
            )
            .map_err(|_| GuardError::Lost)
            .and_then(|()| {
                completion
                    .recv_timeout(timeout)
                    .map_err(|_| GuardError::Lost)?
                    .map_err(|_| GuardError::Lost)
            })
            .and_then(|()| {
                self.receive_ack(
                    request_id,
                    &ResponseResult::SenderGuardAccepted { enrollment },
                    timeout,
                )
            });
        if result.is_err() {
            self.broker.close();
        }
        result?;
        self.live()?;
        let state = self.revocable()?;
        if state.revoked || state.revision != self.scope.revision {
            return Err(GuardError::Authority);
        }
        drop(state);
        self.announced = true;
        Ok(())
    }

    pub(super) fn close_handoff(&mut self, timeout: Duration) -> Result<(), GuardError> {
        let Some(enrollment) = self.handoff.clone() else {
            return Ok(());
        };
        let result = (|| {
            let request = response(
                "guard-close",
                ResponseResult::SenderGuardClosing {
                    enrollment: enrollment.clone(),
                },
            );
            let (sent, completion) = mpsc::sync_channel(1);
            self.broker
                .send(
                    request.canonical_bytes(),
                    Box::new(move |value| {
                        let _ = sent.send(value);
                    }),
                )
                .map_err(|_| GuardError::Cleanup)?;
            completion
                .recv_timeout(timeout)
                .map_err(|_| GuardError::Cleanup)?
                .map_err(|_| GuardError::Cleanup)?;
            self.receive_ack(
                "guard-close",
                &ResponseResult::SenderGuardClosed { enrollment },
                timeout,
            )
        })();
        if result.is_err() {
            self.broker.close();
            // A dead broker has closed every descriptor in its process. A lost
            // channel to a living broker is not proof of endpoint cleanup.
            let mut poll = [rustix::event::PollFd::new(
                &self.broker_pin,
                rustix::event::PollFlags::IN,
            )];
            if rustix::event::poll(&mut poll, Some(&rustix::event::Timespec::default())) != Ok(1) {
                return Err(GuardError::Cleanup);
            }
        }
        self.handoff = None;
        self.announced = false;
        Ok(())
    }

    fn receive_ack(
        &self,
        request_id: &str,
        expected: &ResponseResult,
        timeout: Duration,
    ) -> Result<(), GuardError> {
        let (sent, completion) = mpsc::sync_channel(1);
        self.broker
            .receive(Box::new(move |result| {
                let _ = sent.send(result);
            }))
            .map_err(|_| GuardError::Lost)?;
        let packet = completion
            .recv_timeout(timeout)
            .map_err(|_| GuardError::Lost)?
            .map_err(|_| GuardError::Lost)?;
        let LauncherPacket::Response(accepted) = packet.packet else {
            return Err(GuardError::Authority);
        };
        if accepted.request_id != request_id
            || accepted.result != *expected
            || packet.descriptors.is_some()
            || packet.message_credentials != self.broker.peer_credentials()
        {
            return Err(GuardError::Authority);
        }
        Ok(())
    }
}

fn response(request_id: &str, result: ResponseResult) -> ProtocolResponse {
    ProtocolResponse {
        schema: RESPONSE_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.into(),
        result,
    }
}
