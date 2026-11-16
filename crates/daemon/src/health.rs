//! Standard gRPC health for the whole server: the readiness of wallet tip reads, and the age of
//! the served tip.

use std::{
    fmt,
    time::{Duration, SystemTime},
};

use eyre::{Result, WrapErr as _};
use tokio::{sync::watch, time::MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tonic_health::{ServingStatus, server::HealthReporter};
use tracing::{info, warn};
use ztreamer_sync::head::ServingSnapshot;

use crate::config::ReadinessConfig;

/// The interval of the check of the tip age when no new snapshot comes.
const RECHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Publishes aggregate health (`service = ""`) whenever the serving snapshot changes, and each
/// [`RECHECK_INTERVAL`]: the served tip gets older also when nothing changes.
pub(crate) struct HealthTask {
    reporter: HealthReporter,
    snapshots: watch::Receiver<ServingSnapshot>,
    config: ReadinessConfig,
    /// The last published status. `None` before the first update.
    status: Option<ServingStatus>,
}

impl HealthTask {
    /// Sets the initial status before the gRPC listener opens.
    pub(crate) async fn new(
        reporter: HealthReporter,
        snapshots: watch::Receiver<ServingSnapshot>,
        config: ReadinessConfig,
    ) -> Self {
        let mut task = Self { reporter, snapshots, config, status: None };
        task.update().await;
        task
    }

    /// Follows readiness until shutdown, or fails if the head follower disappears.
    pub(crate) async fn run(mut self, shutdown: CancellationToken) -> Result<()> {
        let mut recheck = tokio::time::interval(RECHECK_INTERVAL);
        recheck.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => {
                    self.stop().await;
                    return Ok(());
                }
                changed = self.snapshots.changed() => {
                    if let Err(error) = changed {
                        self.stop().await;
                        return Err(error).wrap_err("health snapshot source closed");
                    }
                    self.update().await;
                }
                _ = recheck.tick() => self.update().await,
            }
        }
    }

    /// Publishes the status of the current snapshot when it changes, and logs the change.
    async fn update(&mut self) {
        let readiness = check(&self.snapshots.borrow_and_update(), self.config, SystemTime::now());
        let status =
            if readiness.is_ok() { ServingStatus::Serving } else { ServingStatus::NotServing };
        if self.status.replace(status) == Some(status) {
            return;
        }
        match readiness {
            Ok(()) => info!("gRPC health is SERVING"),
            Err(reason) => warn!(%reason, "gRPC health is NOT_SERVING"),
        }
        self.reporter.set_service_status("", status).await;
    }

    async fn stop(&mut self) {
        self.reporter.set_service_status("", ServingStatus::NotServing).await;
        // Drop the sender so Watch streams finish and cannot hold up graceful shutdown.
        self.reporter.clear_service_status("").await;
    }
}

/// The reason why the server is not ready.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NotReady {
    /// Deep reorg recovery runs.
    Recovering,
    /// The last head poll of the node that succeeded is too old.
    NodeSilent,
    /// The snapshot has no block with a header time.
    NoTipTime,
    /// The header time of the served tip is older than the maximum age.
    TipStale { height: u32, age: Duration, max_age: Duration },
}

impl fmt::Display for NotReady {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Recovering => f.write_str("deep reorg recovery runs"),
            Self::NodeSilent => f.write_str("the head poll of the node fails"),
            Self::NoTipTime => f.write_str("the served tip has no header time"),
            Self::TipStale { height, age, max_age } => write!(
                f,
                "the served tip at height {height} is {} s old, more than {} s",
                age.as_secs(),
                max_age.as_secs()
            ),
        }
    }
}

/// The readiness of `snapshot` at `now`. The tip must be fresh
/// ([`Readiness::tip`](ztreamer_sync::head::Readiness::tip)), and the header time of the served
/// tip must be at most [`ReadinessConfig::max_tip_age`] before `now`. A header time after `now`
/// has age zero.
fn check(
    snapshot: &ServingSnapshot,
    config: ReadinessConfig,
    now: SystemTime,
) -> Result<(), NotReady> {
    let readiness = snapshot.readiness();
    if readiness.recovering {
        return Err(NotReady::Recovering);
    }
    if !readiness.tip {
        return Err(NotReady::NodeSilent);
    }
    let Some(max_age) = config.max_tip_age else {
        return Ok(());
    };
    // The volatile head holds the blocks above the durable tip, thus the visible tip.
    let tip = snapshot.volatile_head.last().ok_or(NotReady::NoTipTime)?;
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(u64::from(tip.time));
    let age = now.duration_since(time).unwrap_or_default();
    if age > max_age {
        return Err(NotReady::TipStale { height: tip.height, age, max_age });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::time::timeout;
    use tokio_stream::StreamExt as _;
    use tonic::Request;
    use tonic_health::{
        pb::{
            HealthCheckRequest, health_check_response::ServingStatus as Status,
            health_server::Health as _,
        },
        server::HealthService,
    };
    use tracing::Instrument as _;
    use zakura_chain::block;
    use ztreamer_chain::codec::{CompactBlockRecord, TreeSizes};
    use ztreamer_index::index::IndexState;

    use super::*;

    const NO_AGE_CHECK: ReadinessConfig = ReadinessConfig { max_tip_age: None };

    /// A fresh snapshot with a served tip at height 7 with the header time `time`.
    fn snapshot_with_tip_time(time: u32) -> ServingSnapshot {
        let mut snapshot = ServingSnapshot::from(IndexState::default());
        snapshot.tip_fresh = true;
        snapshot.volatile_head = Arc::from([CompactBlockRecord {
            height: 7,
            hash: block::Hash([7; 32]),
            previous_hash: block::Hash([6; 32]),
            time,
            transactions: Vec::new(),
            end_tree_sizes: TreeSizes::default(),
        }]);
        snapshot
    }

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn the_tip_age_decides_after_the_tip_readiness() {
        let config = ReadinessConfig::default();
        let snapshot = snapshot_with_tip_time(1_000_000);
        assert_eq!(check(&snapshot, config, at(1_000_000)), Ok(()));
        assert_eq!(check(&snapshot, config, at(1_000_300)), Ok(()));
        assert_eq!(
            check(&snapshot, config, at(1_000_301)),
            Err(NotReady::TipStale {
                height: 7,
                age: Duration::from_secs(301),
                max_age: ReadinessConfig::DEFAULT_MAX_TIP_AGE,
            })
        );
        // A header time after the local clock is fresh.
        assert_eq!(check(&snapshot, config, at(999_000)), Ok(()));
        // With no age check, an old tip is ready.
        assert_eq!(check(&snapshot, NO_AGE_CHECK, at(u64::from(u32::MAX))), Ok(()));
    }

    #[test]
    fn a_tip_that_is_not_fresh_is_not_ready_at_any_age() {
        let mut snapshot = snapshot_with_tip_time(1_000_000);
        for config in [ReadinessConfig::default(), NO_AGE_CHECK] {
            snapshot.tip_fresh = false;
            assert_eq!(check(&snapshot, config, at(1_000_000)), Err(NotReady::NodeSilent));
            snapshot.tip_fresh = true;
            snapshot.ready = false;
            assert_eq!(check(&snapshot, config, at(1_000_000)), Err(NotReady::Recovering));
            snapshot.ready = true;
        }
    }

    #[test]
    fn a_snapshot_with_no_volatile_head_is_ready_only_with_no_age_check() {
        let mut snapshot = ServingSnapshot::from(IndexState::default());
        snapshot.tip_fresh = true;
        let now = SystemTime::now();
        assert_eq!(check(&snapshot, ReadinessConfig::default(), now), Err(NotReady::NoTipTime));
        assert_eq!(check(&snapshot, NO_AGE_CHECK, now), Ok(()));
    }

    #[tokio::test]
    async fn check_and_watch_follow_readiness_and_shutdown() {
        let (sender, snapshots) = watch::channel(ServingSnapshot::from(IndexState::default()));
        let reporter = HealthReporter::new();
        let service = HealthService::from_health_reporter(reporter.clone());
        let task = HealthTask::new(reporter, snapshots, NO_AGE_CHECK).await;
        let request = || Request::new(HealthCheckRequest { service: String::new() });
        assert_eq!(
            service.check(request()).await.unwrap().into_inner().status(),
            Status::NotServing
        );
        let mut statuses = service.watch(request()).await.unwrap().into_inner();
        assert_eq!(statuses.next().await.unwrap().unwrap().status(), Status::NotServing);

        let shutdown = CancellationToken::new();
        let running = tokio::spawn(task.run(shutdown.clone()).in_current_span());
        for (ready, tip_fresh, expected) in [
            (true, true, Status::Serving),
            (false, true, Status::NotServing),
            (true, true, Status::Serving),
            (true, false, Status::NotServing),
            (true, true, Status::Serving),
        ] {
            sender.send_modify(|snapshot| {
                snapshot.ready = ready;
                snapshot.tip_fresh = tip_fresh;
            });
            let status =
                timeout(Duration::from_secs(5), statuses.next()).await.unwrap().unwrap().unwrap();
            assert_eq!(status.status(), expected);
            assert_eq!(service.check(request()).await.unwrap().into_inner().status(), expected);
        }
        shutdown.cancel();
        timeout(Duration::from_secs(5), running).await.unwrap().unwrap().unwrap();
        assert_eq!(statuses.next().await.unwrap().unwrap().status(), Status::NotServing);
        assert!(timeout(Duration::from_secs(5), statuses.next()).await.unwrap().is_none());
    }

    /// The tip gets older with no new snapshot. The recheck makes the status `NOT_SERVING`.
    #[tokio::test]
    async fn a_tip_that_gets_old_with_no_new_snapshot_stops_serving() {
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        let (_sender, snapshots) =
            watch::channel(snapshot_with_tip_time(u32::try_from(now).unwrap()));
        let reporter = HealthReporter::new();
        let service = HealthService::from_health_reporter(reporter.clone());
        let config = ReadinessConfig { max_tip_age: Some(Duration::from_secs(2)) };
        let task = HealthTask::new(reporter, snapshots, config).await;
        let request = Request::new(HealthCheckRequest { service: String::new() });
        let mut statuses = service.watch(request).await.unwrap().into_inner();
        assert_eq!(statuses.next().await.unwrap().unwrap().status(), Status::Serving);

        let shutdown = CancellationToken::new();
        let running = tokio::spawn(task.run(shutdown.clone()).in_current_span());
        let status =
            timeout(Duration::from_secs(10), statuses.next()).await.unwrap().unwrap().unwrap();
        assert_eq!(status.status(), Status::NotServing);
        shutdown.cancel();
        timeout(Duration::from_secs(5), running).await.unwrap().unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_closed_snapshot_source_fails_closed() {
        let mut snapshot = ServingSnapshot::from(IndexState::default());
        snapshot.tip_fresh = true;
        let (sender, snapshots) = watch::channel(snapshot);
        let reporter = HealthReporter::new();
        let service = HealthService::from_health_reporter(reporter.clone());
        let task = HealthTask::new(reporter, snapshots, NO_AGE_CHECK).await;
        let request = Request::new(HealthCheckRequest { service: String::new() });
        let mut statuses = service.watch(request).await.unwrap().into_inner();
        assert_eq!(statuses.next().await.unwrap().unwrap().status(), Status::Serving);
        drop(sender);
        assert!(task.run(CancellationToken::new()).await.is_err());
        assert_eq!(statuses.next().await.unwrap().unwrap().status(), Status::NotServing);
        assert!(statuses.next().await.is_none());
    }
}
