//! Bulk and live data loaders (Go `dgraph/cmd/bulk`, `dgraph/cmd/live`).
//!
//! [`live`] streams RDF/JSON files into a live engine in committed
//! batches; [`bulk`] loads files into a fresh engine with schema
//! inference. Both share the UID assignment in [`assign`] (numeric
//! UIDs pass through, blank nodes allocate from above the highest
//! stored UID — the embedded stand-in for Zero UID leases).

pub mod assign;
pub mod bulk;
pub mod live;
