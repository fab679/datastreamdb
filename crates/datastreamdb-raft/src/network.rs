//! openraft transports: in-process channels and tonic TCP.
//!
//! [`ChannelFactory`] routes RPCs between [`GroupRaft`]
//! handles in one process (tests, single-binary clusters).
//! [`TonicFactory`] dials remote nodes over the `RaftTransport` gRPC service
//! (see `proto/raft_transport.proto`); [`TransportServer`] serves it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use openraft::error::{RPCError, StreamingError, Unreachable};
use openraft::impls::BasicNode;
use openraft::network::{RPCOption, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, AppendResult, SnapshotResponse, VoteRequest,
    VoteResponse,
};
use openraft::{RaftNetworkV2, Snapshot};

use crate::state_machine::Applier;
use crate::types::{
    DataStreamDBTypeConfig, GroupRaft, GroupSnapshot, GroupSnapshotMeta, GroupVote,
};

/// Generated `rafttransport` package (messages + client + server).
pub mod transport {
    #![allow(missing_docs, clippy::all, clippy::pedantic)]
    include!(concat!(env!("OUT_DIR"), "/rafttransport.rs"));
}

/// Default snapshot chunk size when the RPC option sets none.
const DEFAULT_CHUNK_SIZE: usize = 1024 * 1024;

/// Builds a backoff-and-retry RPC error for a transport failure.
fn unreachable_err(target: u64, err: impl std::fmt::Display) -> RPCError<DataStreamDBTypeConfig> {
    RPCError::Unreachable(Unreachable::from_string(format!("target {target}: {err}")))
}

/// Builds a snapshot-streaming error for a transport failure.
fn stream_unreachable_err(
    target: u64,
    err: impl std::fmt::Display,
) -> StreamingError<DataStreamDBTypeConfig> {
    StreamingError::Unreachable(Unreachable::from_string(format!("target {target}: {err}")))
}

/// In-process registry routing node IDs to local raft handles.
#[derive(Debug, Clone)]
pub struct ChannelRegistry<A: Applier> {
    /// Live handles by raft ID.
    inner: Arc<Mutex<HashMap<u64, GroupRaft<A>>>>,
}

impl<A: Applier> Default for ChannelRegistry<A> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<A: Applier> ChannelRegistry<A> {
    /// Registers a local raft handle under `id`.
    ///
    /// # Panics
    ///
    /// Panics when the registry lock is poisoned.
    pub fn register(&self, id: u64, raft: GroupRaft<A>) {
        self.inner.lock().expect("registry lock").insert(id, raft);
    }

    /// Drops the handle registered under `id`.
    ///
    /// # Panics
    ///
    /// Panics when the registry lock is poisoned.
    pub fn unregister(&self, id: u64) {
        self.inner.lock().expect("registry lock").remove(&id);
    }

    /// Looks up the handle for `target`.
    fn get(&self, target: u64) -> Option<GroupRaft<A>> {
        self.inner
            .lock()
            .expect("registry lock")
            .get(&target)
            .cloned()
    }
}

/// [`RaftNetworkFactory`] producing in-process [`ChannelNetwork`] clients.
#[derive(Debug, Clone)]
pub struct ChannelFactory<A: Applier> {
    /// Shared handle registry.
    registry: ChannelRegistry<A>,
}

impl<A: Applier> ChannelFactory<A> {
    /// Builds a factory over `registry`.
    #[must_use]
    pub fn new(registry: ChannelRegistry<A>) -> Self {
        Self { registry }
    }
}

impl<A: Applier> RaftNetworkFactory<DataStreamDBTypeConfig> for ChannelFactory<A> {
    type Network = ChannelNetwork<A>;

    async fn new_client(&mut self, target: u64, _node: &BasicNode) -> Self::Network {
        ChannelNetwork {
            target,
            registry: self.registry.clone(),
        }
    }
}

/// In-process [`RaftNetworkV2`]: direct calls into a local raft handle.
#[derive(Debug, Clone)]
pub struct ChannelNetwork<A: Applier> {
    /// Destination raft ID.
    target: u64,
    /// Shared handle registry.
    registry: ChannelRegistry<A>,
}

impl<A: Applier> RaftNetworkV2<DataStreamDBTypeConfig> for ChannelNetwork<A> {
    type SnapshotData = Vec<u8>;

    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<DataStreamDBTypeConfig>,
        _option: RPCOption,
    ) -> Result<AppendResult<DataStreamDBTypeConfig>, RPCError<DataStreamDBTypeConfig>> {
        let Some(raft) = self.registry.get(self.target) else {
            return Err(unreachable_err(self.target, "not registered"));
        };
        raft.append_entries(rpc)
            .await
            .map_err(|err| unreachable_err(self.target, err))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<DataStreamDBTypeConfig>,
        _option: RPCOption,
    ) -> Result<VoteResponse<DataStreamDBTypeConfig>, RPCError<DataStreamDBTypeConfig>> {
        let Some(raft) = self.registry.get(self.target) else {
            return Err(unreachable_err(self.target, "not registered"));
        };
        raft.vote(rpc)
            .await
            .map_err(|err| unreachable_err(self.target, err))
    }

    async fn pre_vote(
        &mut self,
        rpc: VoteRequest<DataStreamDBTypeConfig>,
        _option: RPCOption,
    ) -> Result<VoteResponse<DataStreamDBTypeConfig>, RPCError<DataStreamDBTypeConfig>> {
        let Some(raft) = self.registry.get(self.target) else {
            return Err(unreachable_err(self.target, "not registered"));
        };
        raft.pre_vote(rpc)
            .await
            .map_err(|err| unreachable_err(self.target, err))
    }

    async fn full_snapshot(
        &mut self,
        vote: GroupVote,
        snapshot: GroupSnapshot,
        _cancel: impl futures::Future<Output = openraft::error::ReplicationClosed>
        + openraft::OptionalSend
        + 'static,
        _option: RPCOption,
    ) -> Result<SnapshotResponse<DataStreamDBTypeConfig>, StreamingError<DataStreamDBTypeConfig>>
    {
        let Some(raft) = self.registry.get(self.target) else {
            return Err(stream_unreachable_err(self.target, "not registered"));
        };
        // In-process installs are immediate; cancellation cannot race them.
        raft.install_full_snapshot(vote, snapshot)
            .await
            .map_err(|err| stream_unreachable_err(self.target, err))
    }
}

/// [`RaftNetworkFactory`] dialing remote nodes over tonic.
#[derive(Debug, Clone, Default)]
pub struct TonicFactory {
    /// This node's raft ID (for timeout error attribution).
    this: u64,
}

impl TonicFactory {
    /// Builds a factory for node `this`.
    #[must_use]
    pub fn new(this: u64) -> Self {
        Self { this }
    }
}

impl RaftNetworkFactory<DataStreamDBTypeConfig> for TonicFactory {
    type Network = TonicNetwork;

    async fn new_client(&mut self, target: u64, node: &BasicNode) -> Self::Network {
        TonicNetwork::connect(self.this, target, &node.addr)
    }
}

/// Tonic [`RaftNetworkV2`]: JSON bodies over `RaftTransport` RPCs.
#[derive(Debug, Clone)]
pub struct TonicNetwork {
    /// This node's raft ID.
    this: u64,
    /// Destination raft ID.
    target: u64,
    /// Destination address.
    addr: String,
}

impl TonicNetwork {
    /// Dials `addr` (the client connects lazily on first RPC).
    fn connect(this: u64, target: u64, addr: &str) -> Self {
        Self {
            this,
            target,
            addr: addr.to_owned(),
        }
    }

    /// Fresh client for one RPC (tonic pools the underlying connection).
    fn client(
        &self,
    ) -> Result<
        transport::raft_transport_client::RaftTransportClient<tonic::transport::Channel>,
        tonic::transport::Error,
    > {
        let endpoint = tonic::transport::Endpoint::from_shared(format!("http://{}", self.addr))?;
        Ok(transport::raft_transport_client::RaftTransportClient::new(
            endpoint.connect_lazy(),
        ))
    }

    /// Runs `rpc` under the soft TTL, mapping failures to [`RPCError`].
    async fn call<T>(
        &self,
        option: &RPCOption,
        action: openraft::network::RPCTypes,
        rpc: impl std::future::Future<Output = Result<T, tonic::Status>>,
    ) -> Result<T, RPCError<DataStreamDBTypeConfig>> {
        let ttl = option.soft_ttl();
        let out = tokio::time::timeout(ttl, rpc).await.map_err(|_| {
            RPCError::Timeout(openraft::error::Timeout {
                action,
                id: self.this,
                target: self.target,
                timeout: ttl,
            })
        })?;
        out.map_err(|status| unreachable_err(self.target, status))
    }
}

impl RaftNetworkV2<DataStreamDBTypeConfig> for TonicNetwork {
    type SnapshotData = Vec<u8>;

    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<DataStreamDBTypeConfig>,
        option: RPCOption,
    ) -> Result<AppendResult<DataStreamDBTypeConfig>, RPCError<DataStreamDBTypeConfig>> {
        let prev_log_id = rpc.prev_log_id;
        let last_log_id = rpc.entries.last().map(|entry| entry.log_id).or(prev_log_id);
        let json = serde_json::to_vec(&rpc).map_err(|err| unreachable_err(self.target, err))?;
        let mut client = self
            .client()
            .map_err(|err| unreachable_err(self.target, err))?;
        let response = self
            .call(
                &option,
                openraft::network::RPCTypes::AppendEntries,
                client.append_entries(transport::Envelope { json }),
            )
            .await?;
        let wire: AppendEntriesResponse<DataStreamDBTypeConfig> =
            serde_json::from_slice(&response.into_inner().json)
                .map_err(|err| unreachable_err(self.target, err))?;
        Ok(wire.into_append_result(prev_log_id, last_log_id))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<DataStreamDBTypeConfig>,
        option: RPCOption,
    ) -> Result<VoteResponse<DataStreamDBTypeConfig>, RPCError<DataStreamDBTypeConfig>> {
        let json = serde_json::to_vec(&rpc).map_err(|err| unreachable_err(self.target, err))?;
        let mut client = self
            .client()
            .map_err(|err| unreachable_err(self.target, err))?;
        let response = self
            .call(
                &option,
                openraft::network::RPCTypes::Vote,
                client.vote(transport::Envelope { json }),
            )
            .await?;
        serde_json::from_slice(&response.into_inner().json)
            .map_err(|err| unreachable_err(self.target, err))
    }

    async fn pre_vote(
        &mut self,
        rpc: VoteRequest<DataStreamDBTypeConfig>,
        option: RPCOption,
    ) -> Result<VoteResponse<DataStreamDBTypeConfig>, RPCError<DataStreamDBTypeConfig>> {
        let json = serde_json::to_vec(&rpc).map_err(|err| unreachable_err(self.target, err))?;
        let mut client = self
            .client()
            .map_err(|err| unreachable_err(self.target, err))?;
        let response = self
            .call(
                &option,
                openraft::network::RPCTypes::Vote,
                client.pre_vote(transport::Envelope { json }),
            )
            .await?;
        serde_json::from_slice(&response.into_inner().json)
            .map_err(|err| unreachable_err(self.target, err))
    }

    async fn full_snapshot(
        &mut self,
        vote: GroupVote,
        snapshot: GroupSnapshot,
        cancel: impl futures::Future<Output = openraft::error::ReplicationClosed>
        + openraft::OptionalSend
        + 'static,
        option: RPCOption,
    ) -> Result<SnapshotResponse<DataStreamDBTypeConfig>, StreamingError<DataStreamDBTypeConfig>>
    {
        let vote_json =
            serde_json::to_vec(&vote).map_err(|err| stream_unreachable_err(self.target, err))?;
        let meta_json = serde_json::to_vec(&snapshot.meta)
            .map_err(|err| stream_unreachable_err(self.target, err))?;
        let chunk_size = option
            .snapshot_chunk_size()
            .unwrap_or(DEFAULT_CHUNK_SIZE)
            .max(1);
        let mut frames = vec![transport::SnapshotFrame {
            frame: Some(transport::snapshot_frame::Frame::Header(
                transport::SnapshotHeader {
                    vote_json,
                    meta_json,
                },
            )),
        }];
        frames.extend(
            snapshot
                .snapshot
                .chunks(chunk_size)
                .map(|chunk| transport::SnapshotFrame {
                    frame: Some(transport::snapshot_frame::Frame::Chunk(chunk.to_vec())),
                }),
        );

        let mut client = self
            .client()
            .map_err(|err| stream_unreachable_err(self.target, err))?;
        let target = self.target;
        let send = async move {
            let stream = futures::stream::iter(frames);
            let response = client
                .install_snapshot(stream)
                .await
                .map_err(|status| stream_unreachable_err(target, status))?;
            serde_json::from_slice::<SnapshotResponse<DataStreamDBTypeConfig>>(
                &response.into_inner().json,
            )
            .map_err(|err| stream_unreachable_err(target, err))
        };
        tokio::select! {
            closed = cancel => Err(StreamingError::Closed(closed)),
            result = send => result,
        }
    }
}

/// Tonic `RaftTransport` server dispatching into a local raft handle.
#[derive(Debug, Clone)]
pub struct TransportServer<A: Applier> {
    /// Local raft handle.
    raft: GroupRaft<A>,
}

impl<A: Applier> TransportServer<A> {
    /// Serves RPCs into `raft`.
    #[must_use]
    pub fn new(raft: GroupRaft<A>) -> Self {
        Self { raft }
    }
}

#[tonic::async_trait]
impl<A: Applier> transport::raft_transport_server::RaftTransport for TransportServer<A> {
    async fn vote(
        &self,
        request: tonic::Request<transport::Envelope>,
    ) -> Result<tonic::Response<transport::Envelope>, tonic::Status> {
        let rpc: VoteRequest<DataStreamDBTypeConfig> =
            serde_json::from_slice(&request.into_inner().json)
                .map_err(|err| tonic::Status::invalid_argument(err.to_string()))?;
        let response = self
            .raft
            .vote(rpc)
            .await
            .map_err(|err| tonic::Status::internal(err.to_string()))?;
        let json = serde_json::to_vec(&response)
            .map_err(|err| tonic::Status::internal(err.to_string()))?;
        Ok(tonic::Response::new(transport::Envelope { json }))
    }

    async fn pre_vote(
        &self,
        request: tonic::Request<transport::Envelope>,
    ) -> Result<tonic::Response<transport::Envelope>, tonic::Status> {
        let rpc: VoteRequest<DataStreamDBTypeConfig> =
            serde_json::from_slice(&request.into_inner().json)
                .map_err(|err| tonic::Status::invalid_argument(err.to_string()))?;
        let response = self
            .raft
            .pre_vote(rpc)
            .await
            .map_err(|err| tonic::Status::internal(err.to_string()))?;
        let json = serde_json::to_vec(&response)
            .map_err(|err| tonic::Status::internal(err.to_string()))?;
        Ok(tonic::Response::new(transport::Envelope { json }))
    }

    async fn append_entries(
        &self,
        request: tonic::Request<transport::Envelope>,
    ) -> Result<tonic::Response<transport::Envelope>, tonic::Status> {
        let rpc: AppendEntriesRequest<DataStreamDBTypeConfig> =
            serde_json::from_slice(&request.into_inner().json)
                .map_err(|err| tonic::Status::invalid_argument(err.to_string()))?;
        let result = self
            .raft
            .append_entries(rpc)
            .await
            .map_err(|err| tonic::Status::internal(err.to_string()))?;
        // `AppendResult` has no serde form; the leader rebuilds it from the
        // request log ids plus this response (documented openraft pattern).
        let wire = AppendEntriesResponse::from(result);
        let json =
            serde_json::to_vec(&wire).map_err(|err| tonic::Status::internal(err.to_string()))?;
        Ok(tonic::Response::new(transport::Envelope { json }))
    }

    async fn install_snapshot(
        &self,
        request: tonic::Request<tonic::Streaming<transport::SnapshotFrame>>,
    ) -> Result<tonic::Response<transport::Envelope>, tonic::Status> {
        use futures::StreamExt as _;
        use transport::snapshot_frame::Frame;

        let mut stream = request.into_inner();
        let header = match stream.next().await {
            Some(Ok(transport::SnapshotFrame {
                frame: Some(Frame::Header(header)),
            })) => header,
            Some(Ok(_)) => {
                return Err(tonic::Status::invalid_argument(
                    "first frame must be a header",
                ));
            }
            Some(Err(status)) => return Err(status),
            None => return Err(tonic::Status::invalid_argument("empty snapshot stream")),
        };
        let vote: GroupVote = serde_json::from_slice(&header.vote_json)
            .map_err(|err| tonic::Status::invalid_argument(err.to_string()))?;
        let meta: GroupSnapshotMeta = serde_json::from_slice(&header.meta_json)
            .map_err(|err| tonic::Status::invalid_argument(err.to_string()))?;
        let mut data = Vec::new();
        while let Some(frame) = stream.next().await {
            match frame?.frame {
                Some(Frame::Chunk(chunk)) => data.extend_from_slice(&chunk),
                _ => {
                    return Err(tonic::Status::invalid_argument(
                        "expected chunk frames only",
                    ));
                }
            }
        }
        let snapshot = Snapshot {
            meta,
            snapshot: data,
        };
        let response = self
            .raft
            .install_full_snapshot(vote, snapshot)
            .await
            .map_err(|err| tonic::Status::internal(err.to_string()))?;
        let json = serde_json::to_vec(&response)
            .map_err(|err| tonic::Status::internal(err.to_string()))?;
        Ok(tonic::Response::new(transport::Envelope { json }))
    }
}
