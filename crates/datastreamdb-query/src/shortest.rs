//! Shortest-path queries. Port of `query/shortest.go`: Dijkstra and
//! k-shortest-paths over incrementally expanded adjacency, plus route
//! assembly.
//!
//! Delta from Go: expansion runs inline (Go streams it through a goroutine);
//! the heap is [`std::collections::BinaryHeap`] with inverted ordering.

#![warn(missing_docs)]

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use datastreamdb_proto::{Facets, List as ProtoList};

use crate::errors::{QueryError, Result};
use crate::subgraph::SubGraph;
use crate::vars::copy_filters_recurse;

/// Path metadata attached to result nodes.
#[derive(Clone, Debug, Default)]
pub struct PathMeta {
    /// Total path weight.
    pub weight: f64,
}

/// One route segment.
#[derive(Clone, Debug)]
struct PathInfo {
    /// Node UID.
    uid: u64,
    /// Traversed attribute.
    attr: String,
    /// Edge facets.
    facet: Option<Facets>,
}

/// One route plus its weight.
#[derive(Clone, Debug, Default)]
struct Route {
    /// Segments.
    route: Vec<PathInfo>,
    /// Total weight.
    total_weight: f64,
}

impl Route {
    /// Index of `uid` in the route (-1 when absent).
    fn index_of(&self, uid: u64) -> i64 {
        self.route
            .iter()
            .position(|seg| seg.uid == uid)
            .map_or(-1, |pos| i64::try_from(pos).unwrap_or(i64::MAX))
    }
}

/// Priority-queue entry with stable tie-breaking.
#[derive(Clone, Debug)]
struct QueueItem {
    /// Node UID.
    uid: u64,
    /// Path cost so far.
    cost: f64,
    /// Hop count.
    hop: usize,
    /// Sequence number (heap stability).
    seq: u64,
    /// Path (k-shortest only).
    path: Route,
}

impl PartialEq for QueueItem {
    fn eq(&self, other: &Self) -> bool {
        self.uid == other.uid
    }
}
impl Eq for QueueItem {}

/// Min-heap ordering by cost (ties by sequence).
#[derive(Clone, Debug)]
struct HeapItem(Reverse<(OrderedFloat, u64)>, QueueItem);

/// Total float ordering for the heap.
#[derive(Clone, Copy, Debug, PartialEq)]
struct OrderedFloat(f64);

impl Eq for OrderedFloat {}

impl PartialOrd for OrderedFloat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedFloat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl PartialEq for HeapItem {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for HeapItem {}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

/// Adjacency entry: cost, facets, and attribute.
#[derive(Clone, Debug, Default)]
struct MapItem {
    /// Traversed attribute.
    attr: String,
    /// Edge cost.
    cost: f64,
    /// Edge facets.
    facet: Option<Facets>,
}

/// Dijkstra bookkeeping per UID.
#[derive(Clone, Debug)]
struct NodeInfo {
    /// Best cost/attr/facet.
    item: MapItem,
    /// Parent UID.
    parent: u64,
    /// Heap cost (for decrease-key).
    cost: f64,
}

/// Incremental adjacency expander (sequential port of `expandOut`).
struct Expander<'a> {
    /// Execution context.
    ctx: &'a crate::process::ExecCtx<'a>,
    /// Live frontier nodes (tree addresses stay put behind `Box`).
    exec: Vec<*mut SubGraph>,
    /// Traversed edge count.
    num_edges: u64,
    /// templates for the next level.
    templates: Vec<SubGraph>,
}

impl<'a> Expander<'a> {
    /// Expander over the shortest root's first level.
    fn new(ctx: &'a crate::process::ExecCtx<'a>, sg: &mut SubGraph) -> Self {
        sg.src_uids = Some(ProtoList {
            uids: vec![sg.params.from],
        });
        sg.uid_matrix = vec![ProtoList {
            uids: vec![sg.params.from],
        }];
        sg.dest_uids.clone_from(&sg.src_uids);
        let templates: Vec<SubGraph> = sg
            .children
            .iter()
            .map(|child| child.as_ref().clone())
            .collect();
        let mut exec: Vec<*mut SubGraph> = Vec::new();
        for child in &mut sg.children {
            child.src_uids.clone_from(&sg.dest_uids);
            exec.push(std::ptr::from_mut::<SubGraph>(child.as_mut()));
        }
        Self {
            ctx,
            exec,
            num_edges: 0,
            templates,
        }
    }

    /// Expands one level into the adjacency map (`false` when exhausted).
    fn step(&mut self, adjacency: &mut HashMap<u64, HashMap<u64, MapItem>>) -> Result<bool> {
        for node in &self.exec {
            // SAFETY: nodes are live tree members; use is sequential.
            let node = unsafe { &mut **node };
            crate::process::process_graph(self.ctx, node)?;
        }

        for node in &self.exec {
            let node = unsafe { &mut **node };
            if node.unknown_attr {
                continue;
            }
            node.update_uid_matrix();
            let src_uids = node.src_uids.clone().unwrap_or_default().uids;
            for (midx, from_uid) in src_uids.iter().enumerate() {
                let Some(row) = node.uid_matrix.get(midx) else {
                    continue;
                };
                for (lidx, to_uid) in row.uids.iter().enumerate() {
                    let (cost, facet) = match node.edge_cost(midx, lidx) {
                        Ok((cost, facet)) => (cost, facet),
                        Err(EdgeCost::Skip) => continue,
                        Err(EdgeCost::Fail(err)) => return Err(err),
                    };
                    adjacency.entry(*from_uid).or_default().insert(
                        *to_uid,
                        MapItem {
                            cost,
                            facet,
                            attr: node.attr.clone(),
                        },
                    );
                    self.num_edges += 1;
                }
            }
        }

        if self.num_edges > crate::recurse::LIMIT_QUERY_EDGE {
            return Err(QueryError::Invalid(format!(
                "Exceeded query edge limit = {}. Found {} edges.",
                crate::recurse::LIMIT_QUERY_EDGE,
                self.num_edges
            )));
        }

        let mut out: Vec<*mut SubGraph> = Vec::new();
        for node in &self.exec {
            let node = unsafe { &mut **node };
            if node
                .dest_uids
                .as_ref()
                .is_none_or(|dest| dest.uids.is_empty())
            {
                continue;
            }
            for template in &self.templates {
                let mut fresh = SubGraph::default();
                copy_filters_recurse(&mut fresh, template);
                fresh.src_uids.clone_from(&node.dest_uids);
                if let Some(src) = &mut fresh.src_uids {
                    src.uids.retain(|uid| !adjacency.contains_key(uid));
                }
                node.children.push(Box::new(fresh));
                let last = node.children.len() - 1;
                out.push(std::ptr::from_mut::<SubGraph>(node.children[last].as_mut()));
            }
        }
        if out.is_empty() {
            return Ok(false);
        }
        self.exec = out;
        Ok(true)
    }
}

/// Edge-cost outcome.
enum EdgeCost {
    /// Skip this edge.
    Skip,
    /// Fail the query.
    Fail(QueryError),
}

impl SubGraph {
    /// Edge cost from the single weight facet (default 1.0).
    fn edge_cost(
        &self,
        matrix: usize,
        list: usize,
    ) -> std::result::Result<(f64, Option<Facets>), EdgeCost> {
        let cost = 1.0;
        let Some(row) = self.facets_matrix.get(matrix) else {
            return Ok((cost, None));
        };
        let Some(fcs) = row.facets_list.get(list) else {
            return Err(EdgeCost::Skip);
        };
        if fcs.facets.is_empty() {
            return Err(EdgeCost::Skip);
        }
        if fcs.facets.len() > 1 {
            return Err(EdgeCost::Fail(QueryError::Invalid(format!(
                "Expected 1 but got {} facets",
                fcs.facets.len()
            ))));
        }
        let facet = &fcs.facets[0];
        match facet.val_type {
            1 => {
                let mut buf = [0u8; 8];
                let len = facet.value.len().min(8);
                buf[..len].copy_from_slice(&facet.value[..len]);
                #[allow(clippy::cast_precision_loss, reason = "Go float64(int64) parity")]
                Ok((i64::from_le_bytes(buf) as f64, Some(fcs.clone())))
            }
            2 => {
                let mut buf = [0u8; 8];
                let len = facet.value.len().min(8);
                buf[..len].copy_from_slice(&facet.value[..len]);
                Ok((f64::from_le_bytes(buf), Some(fcs.clone())))
            }
            _ => Err(EdgeCost::Skip),
        }
    }

    /// Runs Dijkstra or k-shortest-paths per `numPaths`.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on invalid queries or expansion failure.
    pub fn shortest_path(&mut self, ctx: &crate::process::ExecCtx<'_>) -> Result<Vec<SubGraph>> {
        if self.params.alias != "shortest" {
            return Err(QueryError::Invalid(
                "Invalid shortest path query".to_owned(),
            ));
        }
        if self.params.from == 0 || self.params.to == 0 {
            return Ok(Vec::new());
        }
        let num_paths = self.params.num_paths;
        if num_paths > 1 {
            return run_k_shortest_paths(ctx, self);
        }
        run_dijkstra(ctx, self)
    }
}

/// K-shortest loopless paths.
fn run_k_shortest_paths(
    ctx: &crate::process::ExecCtx<'_>,
    sg: &mut SubGraph,
) -> Result<Vec<SubGraph>> {
    let num_paths = usize::try_from(sg.params.num_paths).unwrap_or(0);
    let mut heap = BinaryHeap::new();
    let mut seq = 0u64;
    let mut push = |heap: &mut BinaryHeap<HeapItem>, item: QueueItem| {
        seq += 1;
        let mut item = item;
        item.seq = seq;
        heap.push(HeapItem(Reverse((OrderedFloat(item.cost), seq)), item));
    };
    push(
        &mut heap,
        QueueItem {
            uid: sg.params.from,
            cost: 0.0,
            hop: 0,
            seq: 0,
            path: Route {
                route: vec![PathInfo {
                    uid: sg.params.from,
                    attr: String::new(),
                    facet: None,
                }],
                total_weight: 0.0,
            },
        },
    );

    let mut num_hops = 0usize;
    let max_hops =
        usize::try_from(sg.params.explore_depth.unwrap_or(i32::MAX as u64)).unwrap_or(usize::MAX);
    if max_hops == 0 {
        return Ok(Vec::new());
    }
    let (min_weight, max_weight) = (sg.params.min_weight, sg.params.max_weight);
    let mut adjacency: HashMap<u64, HashMap<u64, MapItem>> = HashMap::new();
    let mut expander = Expander::new(ctx, sg);
    let mut stop_expansion = false;
    let mut kroutes: Vec<Route> = Vec::new();

    while let Some(HeapItem(_, item)) = heap.pop() {
        if item.uid == sg.params.to {
            if item.cost < min_weight {
                continue;
            }
            let mut route = item.path.clone();
            route.total_weight = item.cost;
            kroutes.push(route);
            if kroutes.len() == num_paths {
                break;
            }
        }
        if item.hop + 1 > num_hops && num_hops < max_hops && !stop_expansion {
            if !expander.step(&mut adjacency)? {
                stop_expansion = true;
            }
            num_hops += 1;
        }
        let neighbours = adjacency.get(&item.uid).cloned().unwrap_or_default();
        for (to_uid, info) in &neighbours {
            if item.cost + info.cost > max_weight {
                continue;
            }
            if !item.path.route.is_empty() && item.path.index_of(*to_uid) != -1 {
                continue;
            }
            let mut route = item.path.route.clone();
            route.push(PathInfo {
                uid: *to_uid,
                attr: info.attr.clone(),
                facet: info.facet.clone(),
            });
            push(
                &mut heap,
                QueueItem {
                    uid: *to_uid,
                    cost: item.cost + info.cost,
                    hop: item.hop + 1,
                    seq: 0,
                    path: Route {
                        route,
                        total_weight: 0.0,
                    },
                },
            );
            // Like Go, the overflow pop discards the current best entry.
            if i64::try_from(heap.len()).unwrap_or(i64::MAX) > sg.params.max_frontier_size {
                heap.pop();
            }
        }
    }

    if kroutes.is_empty() {
        sg.dest_uids = Some(ProtoList::default());
        return Ok(Vec::new());
    }
    let path: Vec<u64> = kroutes[0].route.iter().map(|seg| seg.uid).collect();
    sg.dest_uids = Some(ProtoList { uids: path });
    Ok(kroutes.iter().map(assemble_k_route).collect())
}

/// Single shortest path (Dijkstra).
#[allow(clippy::too_many_lines, reason = "exact port of Go runDijkstra")]
fn run_dijkstra(ctx: &crate::process::ExecCtx<'_>, sg: &mut SubGraph) -> Result<Vec<SubGraph>> {
    let mut heap = BinaryHeap::new();
    let mut seq = 0u64;
    seq += 1;
    heap.push(HeapItem(
        Reverse((OrderedFloat(0.0), seq)),
        QueueItem {
            uid: sg.params.from,
            cost: 0.0,
            hop: 0,
            seq,
            path: Route::default(),
        },
    ));

    let max_hops =
        usize::try_from(sg.params.explore_depth.unwrap_or(i32::MAX as u64)).unwrap_or(usize::MAX);
    if max_hops == 0 {
        return Ok(Vec::new());
    }
    let mut adjacency: HashMap<u64, HashMap<u64, MapItem>> = HashMap::new();
    let mut expander = Expander::new(ctx, sg);
    let mut dist: HashMap<u64, NodeInfo> = HashMap::new();
    dist.insert(
        sg.params.from,
        NodeInfo {
            item: MapItem::default(),
            parent: 0,
            cost: 0.0,
        },
    );
    let mut num_hops = 0usize;
    let mut stop_expansion = false;

    while let Some(HeapItem(_, item)) = heap.pop() {
        // Skip stale heap entries (a better cost already won).
        if dist
            .get(&item.uid)
            .is_some_and(|info| info.cost < item.cost)
        {
            continue;
        }
        if item.uid == sg.params.to {
            break;
        }
        if num_hops < max_hops && item.hop + 1 > num_hops && !stop_expansion {
            if !expander.step(&mut adjacency)? {
                stop_expansion = true;
            }
            num_hops += 1;
        }
        let neighbours = adjacency.get(&item.uid).cloned().unwrap_or_default();
        for (to_uid, neighbour) in &neighbours {
            let node_cost = item.cost + neighbour.cost;
            if let Some(info) = dist.get(to_uid) {
                if info.cost <= node_cost {
                    continue;
                }
            }
            seq += 1;
            // Like Go, the overflow pop discards the current best entry.
            if i64::try_from(heap.len()).unwrap_or(i64::MAX) > sg.params.max_frontier_size {
                heap.pop();
            }
            heap.push(HeapItem(
                Reverse((OrderedFloat(node_cost), seq)),
                QueueItem {
                    uid: *to_uid,
                    cost: node_cost,
                    hop: item.hop + 1,
                    seq,
                    path: Route::default(),
                },
            ));
            dist.insert(
                *to_uid,
                NodeInfo {
                    parent: item.uid,
                    cost: node_cost,
                    item: MapItem {
                        cost: node_cost,
                        attr: neighbour.attr.clone(),
                        facet: neighbour.facet.clone(),
                    },
                },
            );
        }
    }

    let mut result = Vec::new();
    let mut current = sg.params.to;
    let total_weight = dist.get(&current).map_or(0.0, |info| info.cost);
    for _ in 0..dist.len().max(1) {
        result.push(current);
        if current == sg.params.from {
            break;
        }
        current = dist.get(&current).map_or(0, |info| info.parent);
        if current == 0 {
            break;
        }
    }
    if result.last() != Some(&sg.params.from) {
        sg.dest_uids = Some(ProtoList::default());
        return Ok(Vec::new());
    }
    result.reverse();
    sg.dest_uids = Some(ProtoList {
        uids: result.clone(),
    });
    Ok(vec![assemble_path(&dist, total_weight, &result)])
}

/// Assembles one Dijkstra route into result subgraphs.
fn assemble_path(dist: &HashMap<u64, NodeInfo>, total_weight: f64, result: &[u64]) -> SubGraph {
    assemble_route(
        &result
            .windows(2)
            .map(|pair| {
                let info = &dist[&pair[1]];
                (
                    pair[0],
                    pair[1],
                    info.item.attr.clone(),
                    info.item.facet.clone(),
                )
            })
            .collect::<Vec<_>>(),
        result[0],
        *result.last().expect("path"),
        total_weight,
    )
}

/// Assembles one k-route into result subgraphs.
fn assemble_k_route(route: &Route) -> SubGraph {
    let mut pairs = Vec::new();
    for pair in route.route.windows(2) {
        pairs.push((
            pair[0].uid,
            pair[1].uid,
            pair[1].attr.clone(),
            pair[1].facet.clone(),
        ));
    }
    assemble_route(
        &pairs,
        route.route[0].uid,
        route.route.last().expect("route").uid,
        route.total_weight,
    )
}

/// Assembles route segments into nested result subgraphs.
fn assemble_route(
    pairs: &[(u64, u64, String, Option<Facets>)],
    first: u64,
    last: u64,
    total_weight: f64,
) -> SubGraph {
    let mut root = SubGraph {
        params: crate::subgraph::Params {
            alias: "_path_".to_owned(),
            shortest: true,
            ..crate::subgraph::Params::default()
        },
        path_meta: Some(PathMeta {
            weight: total_weight,
        }),
        src_uids: Some(ProtoList { uids: vec![first] }),
        dest_uids: Some(ProtoList { uids: vec![first] }),
        uid_matrix: vec![ProtoList { uids: vec![first] }],
        ..SubGraph::default()
    };
    let mut current = &mut root;
    for (from, to, attr, facet) in pairs {
        let mut node = SubGraph {
            params: crate::subgraph::Params {
                shortest: true,
                ..crate::subgraph::Params::default()
            },
            ..SubGraph::default()
        };
        if facet.is_some() {
            node.params.facet = Some(datastreamdb_proto::FacetParams::default());
        }
        node.attr.clone_from(attr);
        node.facets_matrix = vec![datastreamdb_proto::FacetsList {
            facets_list: vec![facet.clone().unwrap_or_default()],
        }];
        node.src_uids = Some(ProtoList { uids: vec![*from] });
        node.dest_uids = Some(ProtoList { uids: vec![*to] });
        node.uid_matrix = vec![ProtoList { uids: vec![*to] }];
        current.children.push(Box::new(node));
        let last_idx = current.children.len() - 1;
        // Re-borrow through the just-pushed child.
        current = &mut *current.children[last_idx];
    }
    let mut tail = SubGraph {
        params: crate::subgraph::Params {
            shortest: true,
            ..crate::subgraph::Params::default()
        },
        ..SubGraph::default()
    };
    tail.src_uids = Some(ProtoList { uids: vec![last] });
    tail.uid_matrix = vec![ProtoList { uids: vec![last] }];
    current.children.push(Box::new(tail));
    root
}
