//! The encoded protobuf file descriptor sets of the served protocols, for gRPC server
//! reflection.
//!
//! Each constant is an encoded `google.protobuf.FileDescriptorSet` that `build.rs` writes while
//! it compiles the protobuf definitions. A reflection server decodes it, for example with
//! `tonic_reflection::server::Builder::register_encoded_file_descriptor_set`.

/// The file descriptor set of the Lightwallet protocol: `service.proto` with the
/// `cash.z.wallet.sdk.rpc.CompactTxStreamer` service, and `compact_formats.proto`.
pub const LIGHTWALLET_FILE_DESCRIPTOR_SET: &[u8] =
    tonic::include_file_descriptor_set!("lightwalletd_descriptor");

/// The file descriptor set of the Replicator protocol: `ztreamer/replicate.proto` with the
/// `ztreamer.replicate.Replicator` service.
pub const REPLICATE_FILE_DESCRIPTOR_SET: &[u8] =
    tonic::include_file_descriptor_set!("replicate_descriptor");
