//! Compiles the Lightwallet and Replicator protobuf definitions, and writes their file
//! descriptor sets for gRPC server reflection.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    tonic_prost_build::configure()
        .file_descriptor_set_path(out_dir.join("lightwalletd_descriptor.bin"))
        .compile_protos(
            &["proto/lightwalletd/compact_formats.proto", "proto/lightwalletd/service.proto"],
            &["proto/lightwalletd"],
        )?;
    let wire_dir = out_dir.join("wire");
    std::fs::create_dir_all(&wire_dir)?;
    // Reuse the ordinary request/response messages, substituting only the compact
    // block response. The RPC paths and protobuf schema remain identical.
    tonic_prost_build::configure()
        .build_client(false)
        .out_dir(wire_dir)
        .extern_path(".cash.z.wallet.sdk.rpc", "crate::proto")
        .extern_path(".cash.z.wallet.sdk.rpc.CompactBlock", "crate::EncodedCompactBlock")
        .compile_protos(&["proto/lightwalletd/service.proto"], &["proto/lightwalletd"])?;
    let bytes_dir = out_dir.join("compact_bytes");
    std::fs::create_dir_all(&bytes_dir)?;
    tonic_prost_build::configure()
        .build_client(false)
        .build_server(false)
        .out_dir(bytes_dir)
        .bytes(".")
        .compile_protos(&["proto/lightwalletd/compact_formats.proto"], &["proto/lightwalletd"])?;
    // Range chunks share the blob buffer instead of copying it.
    tonic_prost_build::configure()
        .file_descriptor_set_path(out_dir.join("replicate_descriptor.bin"))
        .bytes(".ztreamer.replicate.RangeChunk.data")
        .compile_protos(&["proto/ztreamer/replicate.proto"], &["proto"])?;
    Ok(())
}
