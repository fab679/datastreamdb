//! Zero `Zero` service (`dgraph/cmd/zero/{zero,raft,oracle}.go`).
//!
//! [`ZeroService`] serves membership (Connect, streams, tablets),
//! leases (UID/timestamp/namespaces), and transaction decisions over a
//! [`ZeroState`], [`ZeroOracle`], and [`ZeroLeases`]. Mutating RPCs require
//! zero leadership; followers reject (callers retry across zeros) instead
//! of Go's leader forwarding.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use datastreamdb_proto::api::{Payload, TxnContext};
use datastreamdb_proto::pb::{
    AssignedIds, ConnectionState, DeleteNsRequest, Group, Member, MembershipState,
    MoveTabletRequest, Num, OracleDelta, RemoveNodeRequest, Status, Tablet, TabletRequest,
    TabletResponse, TxnTimestamps, ZeroProposal, num::LeaseType, zero_server::Zero,
};
use datastreamdb_raft::{Node, NodeError};
use futures::StreamExt as _;
use prost::Message as _;
use tokio::sync::Mutex;

use crate::zero_leases::{LeaseError, ZeroLeases};
use crate::zero_oracle::{ZeroOracle, pred_group};
use crate::zero_state::{ZeroApplier, ZeroState};

/// Maps lease failures to gRPC status.
fn lease_status(err: LeaseError) -> tonic::Status {
    match err {
        LeaseError::NothingToLease => tonic::Status::failed_precondition("Nothing to be leased"),
        LeaseError::NotLeader => tonic::Status::failed_precondition("not the zero leader"),
        LeaseError::Overflow(_) | LeaseError::UnknownType(_) => {
            tonic::Status::invalid_argument(err.to_string())
        }
        LeaseError::Propose(message) => tonic::Status::internal(message),
    }
}

/// Zero service: membership, leases, and transaction decisions.
#[derive(Clone)]
pub struct ZeroService {
    /// Replicated cluster state.
    state: Arc<ZeroState>,
    /// Transaction oracle.
    oracle: Arc<ZeroOracle>,
    /// Lease windows.
    leases: Arc<ZeroLeases>,
    /// Zero raft group.
    node: Node<ZeroApplier>,
    /// Replicas wanted per alpha group (Connect placement).
    num_replicas: u32,
    /// Next raft ID to assign (monotonic with state; shared by clones).
    next_raft_id: Arc<AtomicU64>,
    /// Serializes Connect (Go `connectLock`; shared by clones).
    connect_lock: Arc<Mutex<()>>,
}

impl ZeroService {
    /// Serves `state`/`oracle`/`leases` over `node`.
    pub fn new(
        state: Arc<ZeroState>,
        oracle: Arc<ZeroOracle>,
        leases: Arc<ZeroLeases>,
        node: Node<ZeroApplier>,
        num_replicas: u32,
    ) -> Self {
        let next_raft_id = state.snapshot_state().max_raft_id.saturating_add(1).max(1);
        Self {
            state,
            oracle,
            leases,
            node,
            num_replicas: num_replicas.max(1),
            next_raft_id: Arc::new(AtomicU64::new(next_raft_id)),
            connect_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Proposes bytes, mapping raft errors to status.
    async fn propose(&self, proposal: ZeroProposal) -> Result<Vec<u8>, tonic::Status> {
        self.node
            .propose(proposal.encode_to_vec())
            .await
            .map_err(|err| match err {
                NodeError::NotLeader { .. } => {
                    tonic::Status::failed_precondition("not the zero leader")
                }
                other => tonic::Status::internal(other.to_string()),
            })
    }

    /// Next raft ID, monotonic with the replicated ceiling.
    fn allocate_raft_id(&self) -> u64 {
        let floor = self.state.snapshot_state().max_raft_id.saturating_add(1);
        self.next_raft_id.fetch_max(floor, Ordering::SeqCst);
        self.next_raft_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Builds the Connect proposal for `member` (Go `createProposal`).
    fn connect_proposal(&self, member: &mut Member) -> Option<ZeroProposal> {
        let state = self.state.snapshot_state();
        for group in state.groups.values() {
            if group.members.contains_key(&member.id) {
                return None;
            }
        }
        let mut proposal = ZeroProposal::default();
        if member.id == 0 {
            member.id = self.allocate_raft_id();
            proposal.max_raft_id = member.id;
        } else {
            self.next_raft_id
                .fetch_max(member.id.saturating_add(1), Ordering::SeqCst);
            proposal.max_raft_id = member.id;
        }
        if member.group_id > 0 {
            match state.groups.get(&member.group_id) {
                None => {
                    proposal.member = Some(member.clone());
                    return Some(proposal);
                }
                Some(group) if group.members.contains_key(&member.id) => {
                    proposal.member = Some(member.clone());
                    return Some(proposal);
                }
                Some(group) => {
                    if member.learner || group.members.len() < self.num_replicas as usize {
                        proposal.member = Some(member.clone());
                        return Some(proposal);
                    }
                    if member.force_group_id {
                        proposal.member = Some(member.clone());
                        return Some(proposal);
                    }
                }
            }
        }
        for (gid, group) in &state.groups {
            if group.members.len() < self.num_replicas as usize {
                member.group_id = *gid;
                proposal.member = Some(member.clone());
                return Some(proposal);
            }
        }
        member.group_id = state.groups.keys().max().copied().unwrap_or(0) + 1;
        proposal.member = Some(member.clone());
        Some(proposal)
    }

    /// Current state plus oracle watermark for new subscribers.
    fn membership_snapshot(&self) -> MembershipState {
        self.state.snapshot_state()
    }
}

#[tonic::async_trait]
impl Zero for ZeroService {
    async fn connect(
        &self,
        request: tonic::Request<Member>,
    ) -> Result<tonic::Response<ConnectionState>, tonic::Status> {
        let mut member = request.into_inner();
        let _guard = self.connect_lock.lock().await;
        let state = self.membership_snapshot();

        if member.cluster_info_only {
            return Ok(tonic::Response::new(ConnectionState {
                state: Some(state),
                max_pending: self.oracle.max_decided(),
                ..ConnectionState::default()
            }));
        }
        if member.addr.is_empty() {
            return Err(tonic::Status::invalid_argument(format!(
                "NO_ADDR: No address provided: {member:?}"
            )));
        }
        for removed in &state.removed {
            if removed.group_id != 0 && member.id == removed.id {
                return Err(tonic::Status::failed_precondition(format!(
                    "REUSE_RAFTID: Duplicate Raft ID {} to removed member",
                    member.id
                )));
            }
        }
        for group in state.groups.values() {
            for existing in group.members.values() {
                if existing.addr == member.addr && member.id == 0 {
                    return Ok(tonic::Response::new(ConnectionState {
                        state: Some(state.clone()),
                        member: Some(existing.clone()),
                        ..ConnectionState::default()
                    }));
                }
                if existing.addr == member.addr && existing.id != member.id {
                    return Err(tonic::Status::failed_precondition(format!(
                        "REUSE_ADDR: Duplicate address to existing member: {existing:?}"
                    )));
                }
                // Same ID, new address: Go rejects only proven-healthy
                // duplicates (needs conn pools; M6). Accept the update.
            }
        }

        let Some(proposal) = self.connect_proposal(&mut member) else {
            return Ok(tonic::Response::new(ConnectionState {
                state: Some(state),
                member: Some(member),
                ..ConnectionState::default()
            }));
        };
        let response = self.propose(proposal).await?;
        let member = Member::decode(response.as_slice()).unwrap_or(member);
        Ok(tonic::Response::new(ConnectionState {
            state: Some(self.membership_snapshot()),
            member: Some(member),
            max_pending: self.oracle.max_decided(),
        }))
    }

    async fn update_membership(
        &self,
        request: tonic::Request<Group>,
    ) -> Result<tonic::Response<Payload>, tonic::Status> {
        let group = request.into_inner();
        // Checkpoint watermarks drive Go's snapshot decisions; openraft
        // snapshots by policy, so only member changes replicate.
        for member in group.members.values() {
            let proposal = ZeroProposal {
                member: Some(member.clone()),
                ..ZeroProposal::default()
            };
            self.propose(proposal).await?;
        }
        Ok(tonic::Response::new(Payload {
            data: b"OK".to_vec(),
        }))
    }

    type StreamMembershipStream =
        futures::stream::BoxStream<'static, Result<MembershipState, tonic::Status>>;

    async fn stream_membership(
        &self,
        _request: tonic::Request<Payload>,
    ) -> Result<tonic::Response<Self::StreamMembershipStream>, tonic::Status> {
        // Go watches state; M5 polls the membership counter instead.
        let state = self.state.clone();
        let last = state.snapshot_state().counter;
        let tail = futures::stream::unfold((state, last), move |(state, mut last)| async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let snapshot = state.snapshot_state();
                if snapshot.counter != last {
                    last = snapshot.counter;
                    return Some((Ok(snapshot), (state, last)));
                }
            }
        });
        // Send the current state immediately, then follow the counter.
        let head = self.membership_snapshot();
        let stream: Self::StreamMembershipStream =
            Box::pin(futures::stream::once(async move { Ok(head) }).chain(tail));
        Ok(tonic::Response::new(stream))
    }

    type OracleStream = futures::stream::BoxStream<'static, Result<OracleDelta, tonic::Status>>;

    async fn oracle(
        &self,
        _request: tonic::Request<Payload>,
    ) -> Result<tonic::Response<Self::OracleStream>, tonic::Status> {
        let current = self
            .oracle
            .current_state(self.state.snapshot_state().max_txn_ts);
        let rx = self.oracle.subscribe();
        let oracle = self.oracle.clone();
        let state = self.state.clone();
        let stream = futures::stream::unfold(
            (oracle, state, Some(current), rx),
            move |(oracle, state, head, mut rx)| async move {
                if let Some(head) = head {
                    return Some((Ok(head), (oracle, state, None, rx)));
                }
                match rx.recv().await {
                    Ok(delta) => Some((Ok(delta), (oracle, state, None, rx))),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Resync like a fresh subscriber (Go resubscribes).
                        let max = state.snapshot_state().max_txn_ts;
                        Some((Ok(oracle.current_state(max)), (oracle, state, None, rx)))
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
                }
            },
        );
        let stream: Self::OracleStream = Box::pin(stream);
        Ok(tonic::Response::new(stream))
    }

    async fn should_serve(
        &self,
        request: tonic::Request<Tablet>,
    ) -> Result<tonic::Response<Tablet>, tonic::Status> {
        let mut tablet = request.into_inner();
        if tablet.predicate.is_empty() {
            return Err(tonic::Status::invalid_argument(format!(
                "Tablet predicate is empty in {tablet:?}"
            )));
        }
        if tablet.group_id == 0 && !tablet.read_only {
            return Err(tonic::Status::invalid_argument(format!(
                "Group ID is Zero in {tablet:?}"
            )));
        }
        if let Some(serving) = self.state.serving_tablet(&tablet.predicate) {
            if !tablet.force {
                return Ok(tonic::Response::new(serving));
            }
        }
        if tablet.read_only {
            return Ok(tonic::Response::new(Tablet::default()));
        }
        if datastreamdb_x::is_pre_defined_predicate(&tablet.predicate) {
            // Reserved predicates pin to group 1 (Go parity; our
            // pre-defined set covers Go's reserved names).
            tablet.group_id = 1;
        }
        let proposal = ZeroProposal {
            tablet: Some(tablet.clone()),
            ..ZeroProposal::default()
        };
        // Already-served races resolve to the winner below.
        let _ = self.propose(proposal).await;
        self.state
            .serving_tablet(&tablet.predicate)
            .map(tonic::Response::new)
            .ok_or_else(|| tonic::Status::internal("tablet missing after propose"))
    }

    async fn inform(
        &self,
        request: tonic::Request<TabletRequest>,
    ) -> Result<tonic::Response<TabletResponse>, tonic::Status> {
        let req = request.into_inner();
        for tablet in &req.tablets {
            let proposal = ZeroProposal {
                tablet: Some(tablet.clone()),
                ..ZeroProposal::default()
            };
            let _ = self.propose(proposal).await;
        }
        Ok(tonic::Response::new(TabletResponse {
            tablets: req.tablets,
        }))
    }

    async fn assign_ids(
        &self,
        request: tonic::Request<Num>,
    ) -> Result<tonic::Response<AssignedIds>, tonic::Status> {
        let num = request.into_inner();
        if num.bump && self.node.is_leader().await {
            let typ = LeaseType::try_from(num.r#type)
                .map_err(|_| tonic::Status::invalid_argument("unknown lease type"))?;
            return self
                .leases
                .bump(typ, num.val, &self.state, &self.node)
                .await
                .map(tonic::Response::new)
                .map_err(lease_status);
        }
        self.leases
            .lease(&num, &self.state, &self.node)
            .await
            .map(tonic::Response::new)
            .map_err(lease_status)
    }

    async fn timestamps(
        &self,
        request: tonic::Request<Num>,
    ) -> Result<tonic::Response<AssignedIds>, tonic::Status> {
        let mut num = request.into_inner();
        num.r#type = LeaseType::TxnTs as i32;
        let leased = self
            .leases
            .lease(&num, &self.state, &self.node)
            .await
            .map_err(lease_status)?;
        // Stream the new watermark (Go `storePending`): waiting alphas
        // must learn it before the next commit, not with it.
        self.oracle
            .broadcast_max(self.state.snapshot_state().max_txn_ts);
        Ok(tonic::Response::new(leased))
    }

    async fn commit_or_abort(
        &self,
        request: tonic::Request<TxnContext>,
    ) -> Result<tonic::Response<TxnContext>, tonic::Status> {
        let mut txn = request.into_inner();
        if txn.aborted {
            let proposal = ZeroProposal {
                txn: Some(txn.clone()),
                ..ZeroProposal::default()
            };
            self.propose(proposal).await?;
            txn.commit_ts = 0;
            return Ok(tonic::Response::new(txn));
        }
        if self.oracle.has_conflict(&txn) {
            txn.aborted = true;
            let proposal = ZeroProposal {
                txn: Some(txn.clone()),
                ..ZeroProposal::default()
            };
            self.propose(proposal).await?;
            txn.commit_ts = 0;
            return Ok(tonic::Response::new(txn));
        }
        // Predicate moves abort (Go `checkPreds`): groups named in preds
        // must still serve them.
        for pred in &txn.preds {
            let Some(want) = pred_group(pred) else {
                continue;
            };
            let name = pred.split_once('-').map_or(pred.as_str(), |(_, rest)| rest);
            match self.state.serving_tablet(name) {
                Some(tab) if tab.group_id == want => {}
                _ => {
                    txn.aborted = true;
                    let proposal = ZeroProposal {
                        txn: Some(txn.clone()),
                        ..ZeroProposal::default()
                    };
                    self.propose(proposal).await?;
                    txn.commit_ts = 0;
                    return Ok(tonic::Response::new(txn));
                }
            }
        }
        let leased = self
            .leases
            .lease(
                &Num {
                    val: 1,
                    r#type: LeaseType::TxnTs as i32,
                    ..Num::default()
                },
                &self.state,
                &self.node,
            )
            .await
            .map_err(lease_status)?;
        txn.commit_ts = leased.end_id;
        let proposal = ZeroProposal {
            txn: Some(txn.clone()),
            ..ZeroProposal::default()
        };
        self.propose(proposal).await?;
        Ok(tonic::Response::new(txn))
    }

    async fn try_abort(
        &self,
        request: tonic::Request<TxnTimestamps>,
    ) -> Result<tonic::Response<OracleDelta>, tonic::Status> {
        let req = request.into_inner();
        for start_ts in &req.ts {
            let txn = TxnContext {
                start_ts: *start_ts,
                aborted: true,
                ..TxnContext::default()
            };
            let proposal = ZeroProposal {
                txn: Some(txn),
                ..ZeroProposal::default()
            };
            self.propose(proposal).await?;
        }
        Ok(tonic::Response::new(self.oracle.try_abort(
            &req.ts,
            self.state.snapshot_state().max_txn_ts,
        )))
    }

    async fn delete_namespace(
        &self,
        request: tonic::Request<DeleteNsRequest>,
    ) -> Result<tonic::Response<Status>, tonic::Status> {
        let req = request.into_inner();
        let proposal = ZeroProposal {
            delete_ns: Some(req),
            ..ZeroProposal::default()
        };
        self.propose(proposal).await?;
        Ok(tonic::Response::new(Status::default()))
    }

    async fn remove_node(
        &self,
        request: tonic::Request<RemoveNodeRequest>,
    ) -> Result<tonic::Response<Status>, tonic::Status> {
        let req = request.into_inner();
        if req.group_id == 0 {
            // Zero peer removal goes straight to raft membership.
            self.node
                .remove_voter(req.node_id, false)
                .await
                .map_err(|err| tonic::Status::internal(err.to_string()))?;
            return Ok(tonic::Response::new(Status::default()));
        }
        let state = self.state.snapshot_state();
        let Some(group) = state.groups.get(&req.group_id) else {
            return Err(tonic::Status::not_found(format!(
                "No group with groupId {} found",
                req.group_id
            )));
        };
        if !group.members.contains_key(&req.node_id) {
            return Err(tonic::Status::not_found(format!(
                "No node with nodeId {} found in group {}",
                req.node_id, req.group_id
            )));
        }
        if group.members.len() == 1 && !group.tablets.is_empty() {
            return Err(tonic::Status::failed_precondition(format!(
                "Move all tablets from group {} before removing the last node",
                req.group_id
            )));
        }
        let proposal = ZeroProposal {
            member: Some(Member {
                id: req.node_id,
                group_id: req.group_id,
                am_dead: true,
                ..Member::default()
            }),
            ..ZeroProposal::default()
        };
        self.propose(proposal).await?;
        Ok(tonic::Response::new(Status::default()))
    }

    async fn move_tablet(
        &self,
        _request: tonic::Request<MoveTabletRequest>,
    ) -> Result<tonic::Response<Status>, tonic::Status> {
        Err(tonic::Status::unimplemented("predicate moves are M6 scope"))
    }
}
