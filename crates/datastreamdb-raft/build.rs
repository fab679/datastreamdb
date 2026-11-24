//! Codegen for `raft_transport.proto` (openraft tonic transport).
//!
//! Requires `protoc` in `PATH` at build time (same as `datastreamdb-proto`).

fn main() {
    println!("cargo:rerun-if-changed=proto/raft_transport.proto");

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .build_transport(false)
        .compile_protos(&["proto/raft_transport.proto"], &["proto"])
        .expect("protoc codegen for raft transport failed");
}
