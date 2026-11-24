//! Group join service (`pb.Raft`, minus the etcd protocol).
//!
//! [`RaftService`] lets new alphas join an existing group: the joiner
//! dials the group leader's `JoinCluster`, the leader seats it as a voter
//! via raft membership, and replication backfills it. `Heartbeat` and
//! `RaftMessage` belong to Go's etcd-raft wire protocol and stay
//! `Unimplemented` (openraft groups use `RaftTransport` instead).

use std::sync::Arc;

use datastreamdb_proto::api::Payload;
use datastreamdb_proto::pb::{HealthInfo, RaftBatch, RaftContext, raft_server::Raft as RaftTrait};
use datastreamdb_raft::Node;

use crate::applier::AlphaApplier;
use crate::groups::GroupState;

/// Group join endpoint for one alpha.
pub struct RaftService {
    /// Group raft node (membership changes).
    node: Node<AlphaApplier>,
    /// Membership/tablet view.
    groups: Arc<GroupState>,
}

impl RaftService {
    /// Serves joins for `node`/`groups`.
    pub fn new(node: Node<AlphaApplier>, groups: Arc<GroupState>) -> Self {
        Self { node, groups }
    }
}

#[tonic::async_trait]
impl RaftTrait for RaftService {
    async fn join_cluster(
        &self,
        request: tonic::Request<RaftContext>,
    ) -> Result<tonic::Response<Payload>, tonic::Status> {
        let ctx = request.into_inner();
        if !self.groups.serves_group(ctx.group) {
            return Err(tonic::Status::failed_precondition(format!(
                "this server doesn't serve group id: {}",
                ctx.group
            )));
        }
        if !self.node.is_leader().await {
            // The joiner refreshes membership and retries at the leader.
            return Err(tonic::Status::failed_precondition("not the group leader"));
        }
        // Idempotent seats: re-joining the same member is a no-op success.
        self.node
            .add_voter(ctx.id, ctx.addr.clone())
            .await
            .map_err(|err| tonic::Status::internal(err.to_string()))?;
        Ok(tonic::Response::new(Payload::default()))
    }

    async fn is_peer(
        &self,
        request: tonic::Request<RaftContext>,
    ) -> Result<tonic::Response<datastreamdb_proto::pb::PeerResponse>, tonic::Status> {
        let ctx = request.into_inner();
        Ok(tonic::Response::new(datastreamdb_proto::pb::PeerResponse {
            status: self.groups.serves_group(ctx.group),
        }))
    }

    type HeartbeatStream = futures::stream::Empty<Result<HealthInfo, tonic::Status>>;

    async fn heartbeat(
        &self,
        _request: tonic::Request<Payload>,
    ) -> Result<tonic::Response<Self::HeartbeatStream>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "etcd-raft heartbeats are Go-only; openraft groups use RaftTransport",
        ))
    }

    async fn raft_message(
        &self,
        _request: tonic::Request<tonic::Streaming<RaftBatch>>,
    ) -> Result<tonic::Response<Payload>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "etcd-raft messages are Go-only; openraft groups use RaftTransport",
        ))
    }
}
