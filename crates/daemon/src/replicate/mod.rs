//! The `--replicate-from` startup path of the remote mode: a replica imports the sealed ranges of
//! snapshots or of peers, and the `--remote-rpc` node verifies them. Then the daemon indexes the
//! tail and follows the head from that node.
//!
//! Before it fetches a range, the import refuses:
//!
//! - a node that runs another genesis block than the network of its chain;
//! - a node that prunes the next block that the import needs;
//! - in [`Importer::run`], a manifest whose signature does not verify with the snapshot key, and
//!   a manifest for another network or genesis block than the index.

use std::{fmt, ops::ControlFlow, str::FromStr, sync::Arc};

use ed25519_zebra::VerificationKey;
use eyre::{Result, WrapErr, ensure, eyre};
use http::Uri;
use tokio::sync::watch;
use tracing::{Instrument as _, info, warn};
use zakura_chain::{block::Height, parameters::Network};
use zakura_rpc::client::GetBlockchainInfoResponse;
use ztreamer_import::{
    attest::JsonRpcAttestor,
    fetch::{FetchError, HttpSource, PeerSource, RangeSource},
    importer::{ImportConfig, ImportError, Importer, TrustedNode},
    sample::{JsonRpcSampler, SamplePlan},
};
use ztreamer_index::index::range_meta::NodeId;
use ztreamer_remote::RemoteBackend;
use ztreamer_remote::ZakuraClient;
use ztreamer_snapshot::layout::SNAPSHOT_PUBKEY;

use crate::node::StageContext;

/// The snapshot that ztreamer publishes for mainnet.
const MAINNET_SNAPSHOT: &str = "https://snapshot.ztreamer.sh/mainnet";

/// The source of an import: a `--replicate-from` value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplicaSource {
    /// A snapshot directory below an http:// or https:// base URL.
    Snapshot(Uri),
    /// A peer that serves the Replicator gRPC service at `http://host:port`.
    Peer(Uri),
}

impl FromStr for ReplicaSource {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        let wrong = || format!("{value} is not an http(s) URL or a host:port");
        if value.starts_with("http://") || value.starts_with("https://") {
            return value.parse().map(Self::Snapshot).map_err(|_| wrong());
        }
        let uri: Uri = format!("http://{value}").parse().map_err(|_| wrong())?;
        let host_port = uri.port().is_some() && !value.contains('/');
        host_port.then_some(Self::Peer(uri)).ok_or_else(wrong)
    }
}

impl fmt::Display for ReplicaSource {
    /// The URL of a snapshot, or the host:port of a peer.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Snapshot(base) => write!(f, "{base}"),
            Self::Peer(endpoint) => match endpoint.authority() {
                Some(authority) => write!(f, "{authority}"),
                None => write!(f, "{endpoint}"),
            },
        }
    }
}

impl ReplicaSource {
    /// The snapshot that ztreamer publishes for `network`. Only mainnet has one.
    fn published(network: &Network) -> Result<Self> {
        ensure!(
            matches!(network, Network::Mainnet),
            "no snapshot is published for {network}: give --replicate-from"
        );
        Ok(Self::Snapshot(Uri::from_static(MAINNET_SNAPSHOT)))
    }

    /// The source of ranges at this address. `key` signs a snapshot. A peer gets a connection.
    async fn connect(&self, key: VerificationKey) -> Result<Box<dyn RangeSource>, FetchError> {
        Ok(match self {
            Self::Snapshot(base) => Box::new(HttpSource::new(base.clone(), key)?),
            Self::Peer(endpoint) => Box::new(PeerSource::connect(endpoint.clone()).await?),
        })
    }
}

/// The import that `--replicate-from` asks for.
#[derive(Clone, Debug)]
pub(crate) struct Replica {
    /// `None` for the published snapshot of the network.
    source: Option<ReplicaSource>,
    /// The key that signs the snapshots.
    key: VerificationKey,
}

impl Replica {
    /// The import of `source`, or of the published snapshot. `key`, else the built-in key
    /// [`SNAPSHOT_PUBKEY`], checks its snapshots.
    pub(crate) fn new(source: Option<ReplicaSource>, key: Option<VerificationKey>) -> Self {
        let key = key.unwrap_or_else(|| {
            VerificationKey::try_from(SNAPSHOT_PUBKEY).expect("the built-in key is valid")
        });
        Self { source, key }
    }

    /// Imports the epochs of the manifest above the sealed tip of the index of `stage`, checked
    /// against the node of `client`. Each installed row names the node as `id`. `info` is the
    /// `getblockchaininfo` answer of the node, and the network of `stage` the network of its
    /// chain. Breaks on the shutdown of `stage`.
    ///
    /// The refusals of the module docs come first. An index that is sealed at or above the
    /// manifest needs no import. The import starts at the sealed tip, also inside an epoch of
    /// the manifest. When the index has a sealed tip and the source is out of reach, the daemon
    /// continues without the import. Each other import error stops the daemon. On shutdown, the
    /// import completes the epoch that it installs, and stops.
    pub(crate) async fn import(
        &self,
        client: &ZakuraClient,
        id: NodeId,
        info: &GetBlockchainInfoResponse,
        stage: &StageContext<'_>,
    ) -> Result<ControlFlow<()>> {
        let &StageContext { index, network, shutdown, .. } = stage;
        let source = self.source.clone().map_or_else(|| ReplicaSource::published(network), Ok)?;
        // `RemoteComponents::wallet` checks the chain and the genesis block again, after the
        // import. The import can take hours, and in that time the node can restart on another
        // chain, or another node can answer at the URL. That second check refuses such a node
        // before the daemon serves from it.
        RemoteBackend::connect(client.clone(), network.clone())
            .await
            .wrap_err("check the chain of the remote zakura node")?;
        let sealed = stage.index_state().await?.sealed_through();
        // The samples and the tail read the block bodies from the next block on.
        let next = Height(sealed.map_or(0, |sealed| sealed + 1));
        let first = info.prune_height().unwrap_or(Height::MIN);
        ensure!(
            next >= first,
            "the import needs block {}, but the remote zakura node keeps block bodies only from \
             height {}: use a node that does not prune, or an index that is sealed through \
             height {} or above",
            next.0,
            first.0,
            first.0 - 1
        );

        let ranges = match source.connect(self.key).await {
            Err(error) if sealed.is_some() && out_of_reach(&error) => {
                return Ok(skip_import(&source, error));
            }
            ranges => ranges.wrap_err_with(|| format!("connect to {source}"))?,
        };
        let node = TrustedNode {
            attestor: Arc::new(JsonRpcAttestor::new(client.clone())),
            sampler: Arc::new(JsonRpcSampler::new(client.clone())),
            id,
        };
        let seed = getrandom::u64().map_err(|error| eyre!("random sample seed: {error}"))?;
        let config = ImportConfig {
            network: network.clone(),
            verify_sample: SamplePlan { per_range: SamplePlan::PER_RANGE, seed },
            import_workers: ImportConfig::default_import_workers(),
        };
        info!(%source, sealed_through = sealed, "importing sealed ranges");

        let (stop, stopped) = watch::channel(false);
        let span = tracing::info_span!(
            parent: None,
            "import.run",
            %source,
            sealed_through = sealed,
            otel.status_code = tracing::field::Empty,
            otel.status_description = tracing::field::Empty,
        );
        let importer = Importer::new(ranges, node, Arc::clone(index), config).run(stopped);
        let mut import = tokio::spawn(importer.instrument(span.clone()));
        let result = tokio::select! {
            result = &mut import => result,
            _ = shutdown.cancelled() => {
                info!("stopping the import before shutdown");
                let _ = stop.send(true);
                import.await
            }
        }
        .wrap_err("import task panicked")?;
        let report = match result {
            Err(ImportError::Ahead { index, manifest }) => {
                info!(index, manifest, "the index needs no import");
                return Ok(ControlFlow::Continue(()));
            }
            Err(ImportError::Fetch(error)) if sealed.is_some() && out_of_reach(&error) => {
                return Ok(skip_import(&source, error));
            }
            // The blocking pool stopped: the runtime stops, as after `report.stopped`.
            Err(error @ ImportError::Cancelled(_)) => {
                info!(%error, "the import stops");
                return Ok(ControlFlow::Break(()));
            }
            result => result
                .inspect_err(|error| {
                    span.record("otel.status_code", "ERROR");
                    span.record("otel.status_description", tracing::field::display(error));
                })
                .wrap_err("import the sealed ranges")?,
        };
        info!(?report, "import complete");
        Ok(if report.stopped { ControlFlow::Break(()) } else { ControlFlow::Continue(()) })
    }
}

/// Returns `true` when `error` tells that the source is out of reach: the connection or the
/// transfer failed. An HTTP status, or a wrong manifest, signature or blob, is not one.
const fn out_of_reach(error: &FetchError) -> bool {
    matches!(
        error,
        FetchError::Http { .. }
            | FetchError::Body(_)
            | FetchError::Connect(_)
            | FetchError::Grpc(_)
            | FetchError::Stalled
    )
}

/// Logs that the daemon continues from the sealed tip of the index without `source`.
fn skip_import(source: &ReplicaSource, error: FetchError) -> ControlFlow<()> {
    let error = eyre::Report::new(error);
    warn!(%source, error = %format_args!("{error:#}"), "source out of reach; skipping the import");
    ControlFlow::Continue(())
}

#[cfg(test)]
mod tests;
