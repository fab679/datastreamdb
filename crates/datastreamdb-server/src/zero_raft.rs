//! Zero join service (`pb.Raft` for group 0).
//!
//! [`ZeroRaftService`] seats new zeros: the joiner dials the zero
//! leader's `JoinCluster`, the leader adds it as a voter via raft
//! membership and announces it into the zeros map, and replication
//! backfills it. `Heartbeat` and `RaftMessage` stay `Unimplemented`
//! like the alpha side (openraft uses `RaftTransport` instead).

use datastreamdb_proto::api::Payload;
use datastreamdb_proto::pb::{
    HealthInfo, Member, RaftBatch, RaftContext, ZeroProposal, raft_server::Raft as RaftTrait,
};
use datastreamdb_raft::Node;
use prost::Message as _;

use crate::zero_state::ZeroApplier;

/// Join endpoint for one zero (group 0 only).
pub struct ZeroRaftService {
    /// Zero raft node (membership changes + announcements).
    node: Node<ZeroApplier>,
}

impl ZeroRaftService {
    /// Serves joins for `node`.
    #[must_use]
    pub fn new(node: Node<ZeroApplier>) -> Self {
        Self { node }
    }
}

#[tonic::async_trait]
impl RaftTrait for ZeroRaftService {
    async fn join_cluster(
        &self,
        request: tonic::Request<RaftContext>,
    ) -> Result<tonic::Response<Payload>, tonic::Status> {
        let ctx = request.into_inner();
        if ctx.group != 0 {
            return Err(tonic::Status::failed_precondition(format!(
                "zero serves group 0, not {}",
                ctx.group
            )));
        }
        if !self.node.is_leader().await {
            return Err(tonic::Status::failed_precondition("not the zero leader"));
        }
        // Idempotent seats: re-joining the same member is a no-op success.
        self.node
            .add_voter(ctx.id, ctx.addr.clone())
            .await
            .map_err(|err| tonic::Status::internal(err.to_string()))?;
        // Announce into the zeros map so followers route to the joiner.
        let announce = ZeroProposal {
            member: Some(Member {
                id: ctx.id,
                group_id: 0,
                addr: ctx.addr,
                ..Member::default()
            }),
            ..ZeroProposal::default()
        };
        self.node
            .propose(announce.encode_to_vec())
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
            status: ctx.group == 0,
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
