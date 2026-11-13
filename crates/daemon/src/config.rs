//! The configuration of the daemon. `ztreamerd` makes it from the command line.
//!
//! [`DaemonConfig`] has the configuration of the index, of the sync, of the servers, and the
//! [`Mode`] of the node. The remote mode has its own type, [`RemoteConfig`].

use std::{
    net::SocketAddr,
    num::{NonZeroU32, NonZeroUsize},
    path::PathBuf,
    sync::Arc,
};

use eyre::{Result, WrapErr};
use tonic::transport::{Identity, Server, ServerTlsConfig};
use zakura_chain::parameters::Network;
use ztreamer_index::index::Index;
use ztreamer_service::DEFAULT_MAX_CONCURRENT_RANGES;
use ztreamer_sync::pipeline::PipelineConfig;

use crate::{
    remote::RemoteConfig,
    telemetry::{GrpcServer, GrpcSpan},
};

/// The configuration of the daemon.
#[derive(Debug)]
pub struct DaemonConfig {
    /// The LMDB compact index.
    pub index: IndexConfig,
    /// The block pipeline passes and the head follower.
    pub sync: SyncConfig,
    /// The gRPC server and the metrics endpoint.
    pub servers: ServerConfig,
    /// The node that gives the chain.
    pub mode: Mode,
}

/// The node that gives the chain to the daemon. There are only two modes.
#[derive(Debug)]
pub enum Mode {
    /// A Zakura node in this process.
    Embedded {
        /// Zakura TOML configuration. Zakura defaults and `ZAKURA_*` variables apply when it is
        /// `None`.
        zakura_config: Option<PathBuf>,
    },
    /// An import of sealed ranges, then a remote Zakura node over JSON-RPC. The box keeps the
    /// two variants near the same size.
    Remote(Box<RemoteConfig>),
}

/// The LMDB compact index.
#[derive(Clone, Debug)]
pub struct IndexConfig {
    /// The directory of the index.
    pub dir: PathBuf,
    /// The maximum LMDB map size in bytes.
    pub map_size: NonZeroUsize,
}

impl IndexConfig {
    /// Opens the index on a blocking thread and refuses an index of another network.
    pub(crate) async fn open(&self, network: &Network) -> Result<Arc<Index>> {
        let (path, map_size) = (self.dir.clone(), self.map_size.get());
        let (name, genesis_hash) = (network.to_string(), network.genesis_hash());
        let span = tracing::Span::current();
        let index = tokio::task::spawn_blocking(move || {
            span.in_scope(|| Index::open(path, map_size, &name, genesis_hash))
        })
        .await
        .wrap_err("index open task panicked")??;
        Ok(Arc::new(index))
    }

    /// Opens the index for the chain in its identity row. Refuses a directory with no index.
    pub fn open_existing(&self) -> Result<Index> {
        Ok(Index::open_existing(&self.dir, self.map_size.get())?)
    }
}

/// The block pipeline passes and the head follower.
#[derive(Clone, Copy, Debug)]
pub struct SyncConfig {
    /// The limits of the historical pipeline.
    pub pipeline: PipelineConfig,
    /// Stop after the pass of the catch-up stage, with no head reconcile, no servers and no head
    /// follower.
    pub index_only: bool,
}

/// The gRPC server and the metrics endpoint.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// The listener of the CompactTxStreamer and Replicator gRPC server.
    pub grpc_listen: SocketAddr,
    /// The TLS files of the gRPC server. `None` serves without TLS.
    pub tls: Option<TlsFiles>,
    /// The listener of the Prometheus metrics endpoint.
    pub metrics_listen: SocketAddr,
    /// Answer Ping. Testing only.
    pub ping_very_insecure: bool,
    /// The concurrency limits of the gRPC server.
    pub limits: GrpcLimits,
}

impl ServerConfig {
    /// The gRPC server, with the TLS identity of [`Self::tls`] if it is set, the stream limit
    /// of [`GrpcLimits::max_concurrent_streams`], and a `grpc.request` span for each request.
    /// Reads the TLS files.
    pub(crate) fn grpc_server(&self) -> Result<GrpcServer> {
        let server = Server::builder()
            .max_concurrent_streams(self.limits.max_concurrent_streams.map(NonZeroU32::get));
        let server = match &self.tls {
            None => server,
            Some(TlsFiles { cert: cert_path, key: key_path }) => {
                let cert = std::fs::read(cert_path)
                    .wrap_err_with(|| format!("read TLS certificate {}", cert_path.display()))?;
                let key = std::fs::read(key_path)
                    .wrap_err_with(|| format!("read TLS private key {}", key_path.display()))?;
                server
                    .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert, key)))
                    .wrap_err("configure gRPC TLS")?
            }
        };
        Ok(server.layer(GrpcSpan::layer()))
    }
}

/// The concurrency limits of the gRPC server, as the flags of lightwalletd of the same names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GrpcLimits {
    /// The HTTP/2 streams that one client connection can have open at a time
    /// (`SETTINGS_MAX_CONCURRENT_STREAMS`). It applies to each service on the gRPC listener.
    /// `None` sets no limit.
    pub max_concurrent_streams: Option<NonZeroU32>,
    /// The block-range streams (`GetBlockRange` and `GetBlockRangeNullifiers`) that run at a
    /// time, over all connections. `None` sets no limit.
    pub max_concurrent_ranges: Option<NonZeroUsize>,
}

impl GrpcLimits {
    /// The default of [`Self::max_concurrent_streams`], as in lightwalletd.
    pub const DEFAULT_MAX_CONCURRENT_STREAMS: NonZeroU32 = NonZeroU32::new(100).unwrap();
    /// The default of [`Self::max_concurrent_ranges`]. lightwalletd defaults it to 200.
    pub const DEFAULT_MAX_CONCURRENT_RANGES: NonZeroUsize = DEFAULT_MAX_CONCURRENT_RANGES;
}

impl Default for GrpcLimits {
    /// [`Self::DEFAULT_MAX_CONCURRENT_STREAMS`] streams for each connection and
    /// [`Self::DEFAULT_MAX_CONCURRENT_RANGES`] block-range streams.
    fn default() -> Self {
        Self {
            max_concurrent_streams: Some(Self::DEFAULT_MAX_CONCURRENT_STREAMS),
            max_concurrent_ranges: Some(Self::DEFAULT_MAX_CONCURRENT_RANGES),
        }
    }
}

/// The PEM files of the TLS identity of the gRPC server.
#[derive(Clone, Debug)]
pub struct TlsFiles {
    /// The certificate chain.
    pub cert: PathBuf,
    /// The private key.
    pub key: PathBuf,
}
