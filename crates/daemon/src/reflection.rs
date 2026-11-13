//! gRPC server reflection for the services of the gRPC listener, so that a client such as
//! `grpcurl` can list and describe them without the `.proto` files. The listener serves both
//! versions of the protocol: `grpc.reflection.v1`, and the older `grpc.reflection.v1alpha` that
//! `grpcurl` and other older tools still ask for.

use eyre::{Result, WrapErr as _};
use tonic::transport::server::Router;
use tonic_reflection::{pb, server::Builder};
use ztreamer_protocol::{LIGHTWALLET_FILE_DESCRIPTOR_SET, REPLICATE_FILE_DESCRIPTOR_SET};

/// The encoded descriptor sets of the services of the gRPC listener: the wallet service, the
/// Replicator, standard health, and both versions of reflection itself.
const DESCRIPTOR_SETS: [&[u8]; 5] = [
    LIGHTWALLET_FILE_DESCRIPTOR_SET,
    REPLICATE_FILE_DESCRIPTOR_SET,
    tonic_health::pb::FILE_DESCRIPTOR_SET,
    pb::v1::FILE_DESCRIPTOR_SET,
    pb::v1alpha::FILE_DESCRIPTOR_SET,
];

/// Adds the v1 and v1alpha reflection services to `router`. Fails only if a descriptor set
/// does not decode.
pub(crate) fn add_reflection<L>(router: Router<L>) -> Result<Router<L>> {
    let v1 = builder().build_v1().wrap_err("build the gRPC reflection v1 service")?;
    let v1alpha =
        builder().build_v1alpha().wrap_err("build the gRPC reflection v1alpha service")?;
    Ok(router.add_service(v1).add_service(v1alpha))
}

/// A reflection builder with [`DESCRIPTOR_SETS`]. Each version of reflection then lists both
/// versions, as the listener serves both.
fn builder() -> Builder<'static> {
    DESCRIPTOR_SETS.into_iter().fold(
        Builder::configure().include_reflection_service(false),
        Builder::register_encoded_file_descriptor_set,
    )
}
