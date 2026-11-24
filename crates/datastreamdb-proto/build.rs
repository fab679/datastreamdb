//! Codegen for `api.proto`, `pb.proto`, and `badger.proto`.
//!
//! Requires `protoc` in `PATH` at build time (system dependency, same as the
//! Go build). Messages plus tonic gRPC clients/servers are generated into
//! `OUT_DIR`; `src/lib.rs` includes them as `api`, `pb`, and `badgerpb4`.

fn main() {
    let protos = ["proto/api.proto", "proto/pb.proto", "proto/badger.proto"];
    for proto in &protos {
        println!("cargo:rerun-if-changed={proto}");
    }

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        // The Zero service has an RPC named `Connect`, which would collide
        // with the generated client `connect` constructor. Clients connect
        // via `tonic::transport::Endpoint` + `Client::new` instead.
        .build_transport(false)
        .compile_protos(&protos, &["proto"])
        .expect("protoc codegen for datastreamdb-proto failed");
}
