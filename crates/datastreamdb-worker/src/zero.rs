//! Alpha-to-zero connection (`worker/groups.go` zero pools).
//!
//! [`ZeroConn`] dials the zero leader, runs the membership and oracle
//! streams, and forwards lease/commit/tablet RPCs. Unknown zero addresses
//! refresh from the [`GroupState`] view.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use datastreamdb_proto::api::{Payload, TxnContext};
use datastreamdb_proto::pb::{
    AssignedIds, ConnectionState, Member, MembershipState, MoveTabletRequest, Num, OracleDelta,
    RemoveNodeRequest, Status, Tablet, TabletRequest, TabletResponse, TxnTimestamps,
    zero_client::ZeroClient,
};
use tokio::sync::mpsc;
use tonic::transport::Channel;

use crate::groups::GroupState;

/// Reconnect pause between zero stream attempts.
const RECONNECT_PAUSE: Duration = Duration::from_secs(1);

/// Cached zero client.
#[derive(Clone)]
struct CachedClient {
    /// Connected client.
    client: ZeroClient<Channel>,
}

/// Alpha's connection to the zero group.
pub struct ZeroConn {
    /// This alpha's raft ID.
    my_id: u64,
    /// This alpha's address.
    my_addr: String,
    /// This alpha's group ID.
    my_group: u32,
    /// Membership view (zero addresses + state sink).
    groups: Arc<GroupState>,
    /// Seed zero addresses.
    seeds: Vec<String>,
    /// Cached client (dropped on transport errors).
    cached: Mutex<Option<CachedClient>>,
}

impl ZeroConn {
    /// Connects to `seeds` on behalf of one alpha.
    pub fn new(
        seeds: Vec<String>,
        groups: Arc<GroupState>,
        my_id: u64,
        my_addr: String,
        my_group: u32,
    ) -> Self {
        Self {
            my_id,
            my_addr,
            my_group,
            groups,
            seeds,
            cached: Mutex::new(None),
        }
    }

    /// Known zero addresses: cached view first, then seeds.
    fn addrs(&self) -> Vec<String> {
        let mut addrs = self.groups.zero_addrs();
        for seed in &self.seeds {
            if !addrs.contains(seed) {
                addrs.push(seed.clone());
            }
        }
        addrs
    }

    /// Dials `addr` for the zero service.
    async fn dial(addr: &str) -> Result<ZeroClient<Channel>, tonic::Status> {
        let endpoint = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .map_err(|err| tonic::Status::invalid_argument(err.to_string()))?;
        let channel = endpoint
            .connect()
            .await
            .map_err(|err| tonic::Status::unavailable(err.to_string()))?;
        Ok(ZeroClient::new(channel))
    }

    /// Client for a working zero, trying every known address.
    async fn client(&self) -> Result<ZeroClient<Channel>, tonic::Status> {
        if let Some(cached) = self.cached.lock().expect("zero lock").clone() {
            return Ok(cached.client);
        }
        let mut last_err = tonic::Status::unavailable("no zero address known");
        for addr in self.addrs() {
            match Self::dial(&addr).await {
                Ok(client) => {
                    *self.cached.lock().expect("zero lock") = Some(CachedClient {
                        client: client.clone(),
                    });
                    return Ok(client);
                }
                Err(err) => last_err = err,
            }
        }
        Err(last_err)
    }

    /// Drops the cached client so the next call redials.
    fn drop_client(&self) {
        *self.cached.lock().expect("zero lock") = None;
    }

    /// Runs `call` against zero, redialing once on transport failure.
    async fn call<T, F, Fut>(&self, mut call: F) -> Result<T, tonic::Status>
    where
        F: FnMut(ZeroClient<Channel>) -> Fut,
        Fut: std::future::Future<Output = Result<T, tonic::Status>>,
    {
        let mut client = self.client().await?;
        match call(client.clone()).await {
            Ok(out) => Ok(out),
            Err(status) if is_transport(&status) => {
                self.drop_client();
                client = self.client().await?;
                call(client).await
            }
            Err(status) => Err(status),
        }
    }

    /// Joins the cluster: assigns this alpha its raft ID + membership.
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when no zero answers.
    pub async fn connect(&self) -> Result<ConnectionState, tonic::Status> {
        let member = Member {
            id: self.my_id,
            group_id: self.my_group,
            addr: self.my_addr.clone(),
            ..Member::default()
        };
        self.call(|mut client| {
            let member = member.clone();
            async move {
                client
                    .connect(member)
                    .await
                    .map(tonic::Response::into_inner)
            }
        })
        .await
    }

    /// Pushes this group's tablets to zero (membership sync).
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when no zero answers.
    pub async fn update_membership(
        &self,
        group: datastreamdb_proto::pb::Group,
    ) -> Result<Payload, tonic::Status> {
        self.call(|mut client| {
            let group = group.clone();
            async move {
                client
                    .update_membership(group)
                    .await
                    .map(tonic::Response::into_inner)
            }
        })
        .await
    }

    /// Bumps zero's UID ceiling past `val` (alpha boot).
    ///
    /// Already-past ceilings report success (Go errors `Nothing to be
    /// leased`; bumps are best-effort advancement).
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when no zero answers.
    pub async fn bump_uids(&self, val: u64) -> Result<(), tonic::Status> {
        let num = Num {
            val,
            bump: true,
            r#type: datastreamdb_proto::pb::num::LeaseType::Uid as i32,
            ..Num::default()
        };
        match self.assign_ids(num).await {
            Ok(_) => Ok(()),
            Err(status) if status.message() == "Nothing to be leased" => Ok(()),
            Err(status) => Err(status),
        }
    }

    /// Leases IDs (UIDs, namespaces) from zero.
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when no zero answers or zero refuses.
    pub async fn assign_ids(&self, num: Num) -> Result<AssignedIds, tonic::Status> {
        self.call(|mut client| async move {
            client
                .assign_ids(num)
                .await
                .map(tonic::Response::into_inner)
        })
        .await
    }

    /// Leases transaction timestamps from zero.
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when no zero answers or zero refuses.
    pub async fn timestamps(&self, num: Num) -> Result<AssignedIds, tonic::Status> {
        self.call(|mut client| async move {
            client
                .timestamps(num)
                .await
                .map(tonic::Response::into_inner)
        })
        .await
    }

    /// Commits or aborts one transaction at zero.
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when no zero answers.
    pub async fn commit_or_abort(&self, txn: TxnContext) -> Result<TxnContext, tonic::Status> {
        self.call(|mut client| {
            let txn = txn.clone();
            async move {
                client
                    .commit_or_abort(txn)
                    .await
                    .map(tonic::Response::into_inner)
            }
        })
        .await
    }

    /// Aborts transactions that have not committed.
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when no zero answers.
    pub async fn try_abort(&self, txns: TxnTimestamps) -> Result<OracleDelta, tonic::Status> {
        self.call(|mut client| {
            let txns = txns.clone();
            async move {
                client
                    .try_abort(txns)
                    .await
                    .map(tonic::Response::into_inner)
            }
        })
        .await
    }

    /// Asks zero which group serves a tablet (`ShouldServe`).
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when no zero answers.
    pub async fn should_serve(&self, tablet: Tablet) -> Result<Tablet, tonic::Status> {
        self.call(|mut client| {
            let tablet = tablet.clone();
            async move {
                client
                    .should_serve(tablet)
                    .await
                    .map(tonic::Response::into_inner)
            }
        })
        .await
    }

    /// Informs zero about locally served tablets (`Inform`).
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when no zero answers.
    pub async fn inform(&self, req: TabletRequest) -> Result<TabletResponse, tonic::Status> {
        self.call(|mut client| {
            let req = req.clone();
            async move { client.inform(req).await.map(tonic::Response::into_inner) }
        })
        .await
    }

    /// Removes a node from the cluster.
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when no zero answers.
    pub async fn remove_node(&self, req: RemoveNodeRequest) -> Result<Status, tonic::Status> {
        self.call(|mut client| async move {
            client
                .remove_node(req)
                .await
                .map(tonic::Response::into_inner)
        })
        .await
    }

    /// Moves a tablet to another group.
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when no zero answers.
    pub async fn move_tablet(&self, req: MoveTabletRequest) -> Result<Status, tonic::Status> {
        self.call(|mut client| {
            let req = req.clone();
            async move {
                client
                    .move_tablet(req)
                    .await
                    .map(tonic::Response::into_inner)
            }
        })
        .await
    }

    /// Deletes a namespace cluster-wide.
    ///
    /// # Errors
    ///
    /// Returns [`tonic::Status`] when no zero answers.
    pub async fn delete_namespace(
        &self,
        req: datastreamdb_proto::pb::DeleteNsRequest,
    ) -> Result<Status, tonic::Status> {
        self.call(|mut client| async move {
            client
                .delete_namespace(req)
                .await
                .map(tonic::Response::into_inner)
        })
        .await
    }

    /// Streams membership states into the group view until cancelled.
    ///
    /// Reconnects forever; the caller aborts the task on shutdown.
    pub async fn run_membership_stream(&self) {
        use futures::StreamExt as _;
        loop {
            let Ok(mut client) = self.client().await else {
                tokio::time::sleep(RECONNECT_PAUSE).await;
                continue;
            };
            let Ok(response) = client.stream_membership(Payload::default()).await else {
                self.drop_client();
                tokio::time::sleep(RECONNECT_PAUSE).await;
                continue;
            };
            let mut stream = response.into_inner();
            while let Some(Ok(state)) = stream.next().await {
                self.apply_membership(&state);
            }
            self.drop_client();
            tokio::time::sleep(RECONNECT_PAUSE).await;
        }
    }

    /// Streams oracle deltas to `delta_tx` until cancelled.
    ///
    /// Reconnects forever; the caller aborts the task on shutdown. Lagged
    /// receivers resubscribe through the reconnect loop.
    pub async fn run_oracle_stream(&self, delta_tx: mpsc::Sender<OracleDelta>) {
        use futures::StreamExt as _;
        loop {
            let Ok(mut client) = self.client().await else {
                tokio::time::sleep(RECONNECT_PAUSE).await;
                continue;
            };
            let Ok(response) = client.oracle(Payload::default()).await else {
                self.drop_client();
                tokio::time::sleep(RECONNECT_PAUSE).await;
                continue;
            };
            let mut stream = response.into_inner();
            while let Some(Ok(delta)) = stream.next().await {
                if delta_tx.send(delta).await.is_err() {
                    return;
                }
            }
            self.drop_client();
            tokio::time::sleep(RECONNECT_PAUSE).await;
        }
    }

    /// Installs a streamed membership state.
    fn apply_membership(&self, state: &MembershipState) {
        self.groups.apply_state(state);
    }

    /// Tablets this group currently serves, for membership sync.
    #[must_use]
    pub fn local_tablets(&self) -> HashMap<String, Tablet> {
        self.groups
            .snapshot_state()
            .groups
            .get(&self.my_group)
            .map_or_else(HashMap::new, |group| group.tablets.clone())
    }
}

/// Transport-level failures worth one redial.
fn is_transport(status: &tonic::Status) -> bool {
    matches!(
        status.code(),
        tonic::Code::Unavailable | tonic::Code::Unknown
    )
}
