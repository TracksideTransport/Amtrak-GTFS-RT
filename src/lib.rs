//! Amtrak GTFS-Realtime service library.
//!
//! One process (`run_service`) polls `getTrainsData` once per cycle, builds each
//! generation of TripUpdates, VehiclePositions, and Alerts in memory, supervises the
//! browser worker, and serves the HTTP API. Combined mode is the only mode.

pub mod config;
pub mod endpoints;
pub mod fleet_trains;
pub mod orchestrator;
pub mod schedule;
pub mod serve;
pub mod sources;
pub mod static_addon;
pub mod static_augment;
pub mod static_gtfs;
pub mod static_repair;
pub mod worker;
pub mod writer;

use crate::config::Config;
use crate::endpoints::AnnounceFirstGeneration;
use crate::orchestrator::{GenerationCommitter, StoreGenerationCommitter};
use crate::sources::advisories::WithAdvisories;
use crate::sources::amtrak::AmtrakSource;
use crate::sources::tracks::refresher::run_board_refresher;
use crate::sources::tracks::{AssignmentStore, WithTracks};
use crate::sources::{RtBatch, RtSource, SourceError};
use crate::static_augment::{PlatformTable, SharedPlatforms};
use crate::static_gtfs::{
    bootstrap_static, recover_static, CachedStaticValidator, MobilityDataStaticValidator,
    StaticSnapshotState, StaticStandardsValidator,
};
use crate::writer::GenerationStore;
use async_trait::async_trait;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

/// How long shutdown waits for the browser worker before abandoning it. Inside Docker's default
/// 10-second `docker stop` grace period, with the worker's own `SIGTERM` grace inside this.
const WORKER_STOP_BUDGET: Duration = Duration::from_secs(8);

/// One long-lived activity owned by the service supervisor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceActivity {
    Poller,
    StaticRefresh,
    Http,
    BrowserWorker,
    Supervisor,
}

impl ServiceActivity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Poller => "poller",
            Self::StaticRefresh => "static_refresh",
            Self::Http => "http",
            Self::BrowserWorker => "browser_worker",
            Self::Supervisor => "supervisor",
        }
    }
}

/// Closed, credential-free reason the service must exit non-zero.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceError {
    activity: ServiceActivity,
    outcome: &'static str,
}

impl ServiceError {
    pub(crate) fn stopped(activity: ServiceActivity) -> Self {
        Self {
            activity,
            outcome: "stopped",
        }
    }

    pub(crate) fn failed(activity: ServiceActivity) -> Self {
        Self {
            activity,
            outcome: "failed",
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} activity {} unexpectedly",
            self.activity.as_str(),
            self.outcome
        )
    }
}

impl std::error::Error for ServiceError {}

/// Runs all required activities as one failure domain.
///
/// Any success, error, or panic is unexpected. The supervisor aborts and awaits
/// every sibling before returning a closed error so the process exits non-zero
/// without leaking source, URL, header, or credential text.
pub async fn supervise<P, S, H>(poller: P, static_refresh: S, http: H) -> Result<(), ServiceError>
where
    P: Future<Output = Result<(), ServiceError>> + Send + 'static,
    S: Future<Output = Result<(), ServiceError>> + Send + 'static,
    H: Future<Output = Result<(), ServiceError>> + Send + 'static,
{
    supervise_with(
        poller,
        static_refresh,
        http,
        None::<WorkerActivity<std::future::Pending<Result<(), ServiceError>>>>,
        std::future::pending(),
    )
    .await
}

type ActivityHandle = JoinHandle<Result<(), ServiceError>>;

/// The optional browser-worker activity and the switch that asks it to stop.
pub struct WorkerActivity<W> {
    /// The worker supervisor, usually [`worker::run_worker`].
    pub run: W,
    /// Set to `true` to stop the worker gracefully.
    pub stop: watch::Sender<bool>,
}

/// [`supervise`] plus an optional browser worker and a shutdown signal.
///
/// The worker is one more activity in the failure domain: when it returns (its restart budget is
/// spent), the service stops. When `shutdown` completes, the worker is stopped first, then the
/// other activities are aborted, and the result is `Ok(())`. On any failure the worker is also
/// stopped before the others are aborted, so no Chromium outlives the service.
///
/// # Errors
///
/// Returns the closed [`ServiceError`] of the first activity that stopped, failed, or panicked.
pub async fn supervise_with<P, S, H, W, T>(
    poller: P,
    static_refresh: S,
    http: H,
    worker: Option<WorkerActivity<W>>,
    shutdown: T,
) -> Result<(), ServiceError>
where
    P: Future<Output = Result<(), ServiceError>> + Send + 'static,
    S: Future<Output = Result<(), ServiceError>> + Send + 'static,
    H: Future<Output = Result<(), ServiceError>> + Send + 'static,
    W: Future<Output = Result<(), ServiceError>> + Send + 'static,
    T: Future<Output = ()>,
{
    let mut tasks = JoinSet::new();
    let mut activities = HashMap::new();
    activities.insert(tasks.spawn(poller).id(), ServiceActivity::Poller);
    activities.insert(
        tasks.spawn(static_refresh).id(),
        ServiceActivity::StaticRefresh,
    );
    activities.insert(tasks.spawn(http).id(), ServiceActivity::Http);
    let mut worker: Option<(ActivityHandle, watch::Sender<bool>)> =
        worker.map(|activity| (tokio::spawn(activity.run), activity.stop));

    let mut worker_finished = false;
    let failure = tokio::select! {
        joined = tasks.join_next_with_id() => Some(match joined {
            Some(Ok((id, Ok(())))) => ServiceError::stopped(
                activities
                    .get(&id)
                    .copied()
                    .unwrap_or(ServiceActivity::Supervisor),
            ),
            Some(Ok((_id, Err(error)))) => error,
            Some(Err(error)) => ServiceError::failed(
                activities
                    .get(&error.id())
                    .copied()
                    .unwrap_or(ServiceActivity::Supervisor),
            ),
            None => ServiceError::failed(ServiceActivity::Supervisor),
        }),
        result = async {
            match worker.as_mut() {
                Some((handle, _)) => handle.await,
                None => std::future::pending().await,
            }
        } => {
            worker_finished = true;
            Some(match result {
                Ok(Ok(())) => ServiceError::stopped(ServiceActivity::BrowserWorker),
                Ok(Err(error)) => error,
                Err(_) => ServiceError::failed(ServiceActivity::BrowserWorker),
            })
        }
        () = shutdown => {
            tracing::info!("shutdown requested; stopping");
            None
        }
    };
    if let Some((mut handle, stop)) = worker {
        if !worker_finished {
            let _ = stop.send(true);
            if tokio::time::timeout(WORKER_STOP_BUDGET, &mut handle)
                .await
                .is_err()
            {
                handle.abort();
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Completes on `SIGTERM` or `SIGINT`.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

struct CapitalCorridorFiltered<S>(S);

#[async_trait]
impl<S> RtSource for CapitalCorridorFiltered<S>
where
    S: RtSource + Send + Sync,
{
    fn name(&self) -> &'static str {
        self.0.name()
    }

    async fn fetch(&self, gtfs: &gtfs_structures::Gtfs) -> Result<RtBatch, SourceError> {
        let batch = self.0.fetch(gtfs).await?;
        Ok(RtBatch {
            trip_updates: amtrak_gtfs_rt::filter_capital_corridor(batch.trip_updates),
            vehicle_positions: amtrak_gtfs_rt::filter_capital_corridor(batch.vehicle_positions),
            alerts: amtrak_gtfs_rt::filter_capital_corridor(batch.alerts),
        })
    }
}

async fn initial_snapshots(
    store: &GenerationStore,
    static_url: &str,
    validator: &dyn StaticStandardsValidator,
    platforms: &[PlatformTable],
) -> Result<StaticSnapshotState, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(generation) = store.current().await {
        return Ok(recover_static(
            generation.static_zip.clone(),
            generation.manifest.static_version.clone(),
        )?);
    }
    Ok(bootstrap_static(static_url, validator, platforms).await?)
}

/// `--healthcheck`: `/livez` must answer `200`, and when the service runs the built-in browser
/// worker, the worker's listener must answer HTTP at all. Any status counts for the worker: its
/// `/healthz` turns `503` while Amtrak rejects polls, and that is a source outage the service
/// already fails open on, not a dead process. A worker past its restart budget has already
/// stopped the service.
fn container_healthcheck() -> std::io::Result<()> {
    let bind_address =
        std::env::var("AMTRAK_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let address = healthcheck_address(&bind_address)?;
    let status = http_status_line(address, "/livez")?;
    if !(status.starts_with(b"HTTP/1.1 200 ") || status.starts_with(b"HTTP/1.0 200 ")) {
        return Err(std::io::Error::other("/livez did not return HTTP 200"));
    }
    if embedded_worker_plan().is_some() {
        let worker = worker_address()?;
        let status = http_status_line(worker, "/healthz")
            .map_err(|_| std::io::Error::other("the browser worker is not answering"))?;
        if !status.starts_with(b"HTTP/1.") {
            return Err(std::io::Error::other("the browser worker is not answering"));
        }
    }
    Ok(())
}

/// First bytes of the response to `GET path` on `address`, with a 4-second bound on each step.
fn http_status_line(address: SocketAddr, path: &str) -> std::io::Result<Vec<u8>> {
    let timeout = Duration::from_secs(4);
    let mut stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut response = [0_u8; 64];
    let bytes_read = stream.read(&mut response)?;
    Ok(response[..bytes_read].to_vec())
}

/// The built-in worker this process starts, from the same environment `run_combined` reads.
fn embedded_worker_plan() -> Option<worker::WorkerPlan> {
    let tracks = crate::config::TrackConfig::from_env().ok()?;
    let advisories = crate::config::AdvisoryConfig::from_env().ok()?;
    worker::WorkerPlan::from_configs(
        &tracks,
        &advisories,
        std::path::Path::new(worker::DEFAULT_WORKER_ROOT),
    )
}

fn worker_address() -> std::io::Result<SocketAddr> {
    crate::config::WORKER_BASE_URL
        .trim_start_matches("http://")
        .parse()
        .map_err(|_| std::io::Error::other("invalid built-in worker address"))
}

/// Shared track state: the store the board refresher fills, the platform table the static feed
/// was augmented with, and the oldest assignment age that may be stamped.
struct TrackWiring {
    store: Arc<AssignmentStore>,
    table: SharedPlatforms,
    max_age: Duration,
}

/// Layers the optional advisory and track decorators. Tracks sit outside advisories so a board
/// failure cannot hide an advisory merge, and the published source name stays the inner source's.
fn realtime_source<S>(
    source: S,
    advisories: Option<crate::config::AdvisoryConfig>,
    tracks: Option<TrackWiring>,
) -> Box<dyn RtSource>
where
    S: RtSource + 'static,
{
    match (advisories, tracks) {
        (None, None) => Box::new(source),
        (Some(advisories), None) => Box::new(WithAdvisories::new(source, advisories)),
        (None, Some(tracks)) => Box::new(WithTracks::new(
            source,
            tracks.store,
            tracks.table,
            tracks.max_age,
        )),
        (Some(advisories), Some(tracks)) => Box::new(WithTracks::new(
            WithAdvisories::new(source, advisories),
            tracks.store,
            tracks.table,
            tracks.max_age,
        )),
    }
}

fn healthcheck_address(bind_address: &str) -> std::io::Result<SocketAddr> {
    let configured: SocketAddr = bind_address.parse().map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid AMTRAK_BIND_ADDR for healthcheck: {error}"),
        )
    })?;
    Ok(SocketAddr::from(([127, 0, 0, 1], configured.port())))
}

/// Starts the service, or probes it with `--healthcheck`.
///
/// # Errors
///
/// Returns a configuration, static-snapshot, or server error, including a leftover
/// `AMTRAK_ROLE` from the three-container layout. `--healthcheck` returns an I/O error
/// when `/livez` is not `200` or the browser worker it runs is not answering.
pub async fn run_service() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--healthcheck")) {
        container_healthcheck()?;
        return Ok(());
    }
    tracing_subscriber::fmt::try_init().ok();
    check_role(std::env::var("AMTRAK_ROLE").ok().as_deref())?;
    run_combined().await
}

/// `AMTRAK_ROLE` is gone with the three-container layout. Unset, empty, or `combined` is
/// accepted; any other value refuses to start instead of silently running every feed.
///
/// # Errors
///
/// Returns [`config::ConfigError`] for `AMTRAK_ROLE`, pointing at the upgrade note.
pub fn check_role(role: Option<&str>) -> Result<(), config::ConfigError> {
    match role.map(str::trim) {
        None | Some("") | Some("combined") => Ok(()),
        Some(_) => Err(config::ConfigError::removed_role()),
    }
}

pub async fn run_combined() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Validation and durable recovery finish before any task or listener exists.
    let config = Config::from_env()?.validate()?;
    let store =
        GenerationStore::open_with_retention(&config.output_dir, config.generation_retention)
            .await?;
    let validator: Arc<dyn StaticStandardsValidator> = Arc::new(CachedStaticValidator::new(
        MobilityDataStaticValidator::new(config.gtfs_validator_jar.clone()),
        config.output_dir.join("validator-cache"),
        config.validator_cache,
    ));
    // With tracks on, every static snapshot is augmented with the configured platform stops, so
    // the same table must reach both the bootstrap below and the periodic refresh task.
    let track_config = crate::config::TrackConfig::from_env()?;
    let observed_platforms = config
        .output_dir
        .join("tracks")
        .join("observed-platforms.txt");
    let platforms = track_config
        .enabled
        .then(|| SharedPlatforms::load(track_config.platforms.clone(), observed_platforms));
    let bootstrap_tables = platforms
        .as_ref()
        .map(SharedPlatforms::candidates)
        .unwrap_or_default();
    let snapshots = initial_snapshots(
        &store,
        &config.static_url,
        validator.as_ref(),
        &bootstrap_tables,
    )
    .await?;
    if let Some(platforms) = &platforms {
        let active = snapshots.active().await;
        platforms.set_known_stations(active.parsed.stops.keys().map(String::as_str));
    }

    // The Amtrak source (optionally Capital Corridor-filtered) is wrapped with the best-effort
    // advisory scraper and the track board. Both are on by default, read the built-in browser
    // worker, and fail open, so a missing worker never risks generation publication. Their
    // flags (`AMTRAK_ADVISORIES`, `AMTRAK_TRACKS`, `AMTRAK_TRACKS_AMTRAK`, ...) turn them off.
    let advisory_config = crate::config::AdvisoryConfig::from_env()?;
    // The built-in browser worker runs only for the browser sources that read it, and only when
    // its package is installed (the container image). Elsewhere those sources fail open.
    let worker_root = std::path::Path::new(worker::DEFAULT_WORKER_ROOT);
    let fleet_trains_file = config.output_dir.join("tracks").join("fleet-trains.json");
    let worker_plan =
        worker::WorkerPlan::from_configs(&track_config, &advisory_config, worker_root).map(
            |plan| {
                plan.with_fleet_trains_file(fleet_trains_file.clone())
                    .with_output_dir(config.output_dir.clone())
            },
        );
    if worker_plan.is_none()
        && worker::WorkerPlan::wanted(&track_config, &advisory_config, worker_root).is_some()
    {
        tracing::warn!(
            "the browser worker is not installed here; Amtrak track numbers and notices fail open"
        );
    }
    let advisories = advisory_config.enabled.then_some(advisory_config);
    let banner_advisories = advisories.clone();
    if track_config.enabled && track_config.amtrak_enabled && track_config.amtrak_url.is_none() {
        tracing::warn!(
            source = "amtrak",
            "AMTRAK_TRACKS_AMTRAK_URL is empty; Amtrak's own track numbers are not published"
        );
    }
    // With tracks on, the board refresher runs as its own task and generation only reads the
    // store it fills, so a slow board never delays a feed. Its RailData token cache lives beside,
    // not inside, the generation directory that the API serves from.
    let tracks = platforms.clone().map(|table| {
        let assignments = Arc::new(AssignmentStore::new());
        let max_age = track_config.max_age;
        tokio::spawn(run_board_refresher(
            assignments.clone(),
            track_config.clone(),
            config.output_dir.join("tracks").join("raildata-token.json"),
            table.clone(),
        ));
        TrackWiring {
            store: assignments,
            table,
            max_age,
        }
    });
    let source: Box<dyn RtSource> = if config.filter_capital_corridor {
        realtime_source(
            CapitalCorridorFiltered(AmtrakSource::new()),
            advisories,
            tracks,
        )
    } else {
        realtime_source(AmtrakSource::new(), advisories, tracks)
    };
    let sources: Arc<Vec<Box<dyn RtSource>>> = Arc::new(vec![source]);
    let base_url = endpoints::base_url(config.bind_addr, config.public_url.as_deref());
    // The worker's track poller reads the train list from this file instead of polling the map.
    let train_list = worker_plan
        .as_ref()
        .is_some_and(|plan| plan.tracks)
        .then_some(fleet_trains_file);
    let committer: Arc<dyn GenerationCommitter> = Arc::new(AnnounceFirstGeneration::new(
        fleet_trains::WithFleetTrainsFile::new(
            StoreGenerationCommitter::new(config.output_dir.clone(), store.clone()),
            train_list,
        ),
        base_url.clone(),
    ));
    let banner = endpoints::startup_banner(
        &base_url,
        config.bind_addr,
        &config.access_policy,
        banner_advisories.as_ref(),
        track_config.enabled.then_some(&track_config),
        config.filter_capital_corridor,
    );
    let mut state = serve::AppState::new(
        store,
        config.access_policy,
        config.freshness_limit,
        Arc::new(serve::SystemClock),
    );
    // A `train` or `station` query on trip updates asks the worker's track poller for an early,
    // debounced refresh. The response never waits: the hook only records the request.
    if worker_plan.as_ref().is_some_and(|plan| plan.tracks) {
        let early = crate::config::EarlyRefreshConfig::from_env()?;
        let scheduler = Arc::new(std::sync::Mutex::new(
            schedule::TrackScheduler::from_config(&early),
        ));
        tokio::spawn(schedule::run_early_hint_writer(
            scheduler.clone(),
            config.output_dir.clone(),
        ));
        state = state.with_trip_interest(schedule::trip_interest_hook(scheduler));
    }
    let app = serve::router(state);
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    println!("{banner}");

    let poller_snapshots = snapshots.clone();
    let refresh_snapshots = snapshots;
    let poll_interval = config.poll_interval;
    let static_url = config.static_url;
    let static_refresh_interval = config.static_refresh_interval;
    let worker = worker_plan.map(|plan| {
        tracing::info!(sources = plan.describe(), "starting the browser worker");
        let (stop, stopped) = watch::channel(false);
        WorkerActivity {
            run: worker::run_worker(
                move || plan.command(),
                worker::RestartPolicy::default(),
                stopped,
            ),
            stop,
        }
    });
    supervise_with(
        async move {
            orchestrator::run_poller(sources, poller_snapshots, committer, poll_interval).await;
            Ok(())
        },
        async move {
            static_gtfs::run_snapshot_refresh(
                refresh_snapshots,
                static_url,
                static_refresh_interval,
                validator,
                platforms,
            )
            .await;
            Ok(())
        },
        async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .map_err(|_| ServiceError::failed(ServiceActivity::Http))
        },
        worker,
        shutdown_signal(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::{EntityCounts, RefreshStage, StaticSnapshot, ValidatedGeneration};
    use crate::sources::mock::{Behavior, MockSource};
    use crate::static_gtfs::StaticValidationError;
    use crate::writer::GenerationPublisher;
    use gtfs_realtime::{FeedHeader, FeedMessage};
    use prost::Message;
    use std::io::{Cursor, Write};
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use zip::write::SimpleFileOptions;

    struct CancellationFlag(Arc<AtomicBool>);

    impl Drop for CancellationFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    type TestFuture = Pin<Box<dyn Future<Output = Result<(), ServiceError>> + Send + 'static>>;

    fn activity_future(
        selected: bool,
        result: Result<(), ServiceError>,
        dropped: Arc<AtomicBool>,
    ) -> TestFuture {
        let guard = CancellationFlag(dropped);
        if selected {
            Box::pin(async move {
                let _guard = guard;
                result
            })
        } else {
            Box::pin(async move {
                let _guard = guard;
                std::future::pending().await
            })
        }
    }

    fn output_directory(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "amtrak-composition-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn static_zip() -> Arc<[u8]> {
        let cursor = Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(cursor);
        let options = SimpleFileOptions::default();
        for (name, contents) in [
            ("agency.txt", "agency_id,agency_name,agency_url,agency_timezone\na,Amtrak,https://amtrak.com,America/New_York\n"),
            ("stops.txt", "stop_id,stop_name,stop_lat,stop_lon\ns,Station,42,-71\n"),
            ("routes.txt", "route_id,agency_id,route_short_name,route_long_name,route_type\nr,a,R,Regional,2\n"),
            ("trips.txt", "route_id,service_id,trip_id\nr,svc,t\n"),
            ("stop_times.txt", "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nt,10:00:00,10:00:00,s,1\n"),
            ("calendar_dates.txt", "service_id,date,exception_type\nsvc,20260813,1\n"),
        ] {
            archive.start_file(name, options).unwrap();
            archive.write_all(contents.as_bytes()).unwrap();
        }
        Arc::from(archive.finish().unwrap().into_inner())
    }

    fn encoded_feed(timestamp: u64, version: &str) -> Arc<[u8]> {
        Arc::from(
            FeedMessage {
                header: FeedHeader {
                    gtfs_realtime_version: "2.0".into(),
                    incrementality: None,
                    timestamp: Some(timestamp),
                    feed_version: Some(version.into()),
                },
                entity: Vec::new(),
            }
            .encode_to_vec(),
        )
    }

    struct RejectIfCalled;

    #[async_trait]
    impl StaticStandardsValidator for RejectIfCalled {
        async fn validate(&self, _zip: Arc<[u8]>) -> Result<(), StaticValidationError> {
            panic!("recovery must not invoke the network static validator")
        }
    }

    #[tokio::test]
    async fn every_activity_success_and_failure_cancels_and_awaits_siblings() {
        for activity in [
            ServiceActivity::Poller,
            ServiceActivity::StaticRefresh,
            ServiceActivity::Http,
        ] {
            for fails in [false, true] {
                let dropped = [
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(AtomicBool::new(false)),
                ];
                let selected_result = if fails {
                    Err(ServiceError::failed(activity))
                } else {
                    Ok(())
                };
                let result = supervise(
                    activity_future(
                        activity == ServiceActivity::Poller,
                        selected_result.clone(),
                        dropped[0].clone(),
                    ),
                    activity_future(
                        activity == ServiceActivity::StaticRefresh,
                        selected_result.clone(),
                        dropped[1].clone(),
                    ),
                    activity_future(
                        activity == ServiceActivity::Http,
                        selected_result,
                        dropped[2].clone(),
                    ),
                )
                .await;
                let expected = if fails {
                    ServiceError::failed(activity)
                } else {
                    ServiceError::stopped(activity)
                };
                assert_eq!(result, Err(expected));
                assert!(dropped.iter().all(|flag| flag.load(Ordering::SeqCst)));
            }
        }
    }

    #[tokio::test]
    async fn panic_names_the_activity_and_cancels_siblings() {
        let refresh_cancelled = Arc::new(AtomicBool::new(false));
        let http_cancelled = Arc::new(AtomicBool::new(false));
        let result = supervise(
            async {
                panic!("fixture panic");
                #[allow(unreachable_code)]
                Ok(())
            },
            activity_future(false, Ok(()), refresh_cancelled.clone()),
            activity_future(false, Ok(()), http_cancelled.clone()),
        )
        .await;
        assert_eq!(result, Err(ServiceError::failed(ServiceActivity::Poller)));
        assert!(refresh_cancelled.load(Ordering::SeqCst));
        assert!(http_cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_spent_worker_budget_fails_the_service_and_cancels_siblings() {
        let dropped = [
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        ];
        let (stop, _stopped) = watch::channel(false);
        let result = supervise_with(
            activity_future(false, Ok(()), dropped[0].clone()),
            activity_future(false, Ok(()), dropped[1].clone()),
            activity_future(false, Ok(()), dropped[2].clone()),
            Some(WorkerActivity {
                run: async { Err(ServiceError::failed(ServiceActivity::BrowserWorker)) },
                stop,
            }),
            std::future::pending(),
        )
        .await;
        assert_eq!(
            result,
            Err(ServiceError::failed(ServiceActivity::BrowserWorker))
        );
        assert!(dropped.iter().all(|flag| flag.load(Ordering::SeqCst)));
    }

    #[tokio::test]
    async fn shutdown_stops_the_worker_before_the_other_activities() {
        let dropped = Arc::new(AtomicBool::new(false));
        let worker_saw_service_running = Arc::new(AtomicBool::new(false));
        let (stop, mut stopped) = watch::channel(false);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let poller_dropped = dropped.clone();
        let observed = worker_saw_service_running.clone();
        let supervisor = tokio::spawn(supervise_with(
            activity_future(false, Ok(()), poller_dropped),
            activity_future(false, Ok(()), Arc::new(AtomicBool::new(false))),
            activity_future(false, Ok(()), Arc::new(AtomicBool::new(false))),
            Some(WorkerActivity {
                run: async move {
                    let _ = stopped.changed().await;
                    // The worker is asked to stop while the service's activities still run.
                    observed.store(!dropped.load(Ordering::SeqCst), Ordering::SeqCst);
                    Ok(())
                },
                stop,
            }),
            async {
                let _ = shutdown_rx.await;
            },
        ));
        shutdown_tx.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), supervisor)
            .await
            .expect("shutdown finished within the budget")
            .unwrap();
        assert_eq!(result, Ok(()));
        assert!(worker_saw_service_running.load(Ordering::SeqCst));
    }

    /// The API answers from the warm generation while the worker is down and backing off.
    #[cfg(unix)]
    #[tokio::test]
    async fn http_answers_while_the_browser_worker_is_down() {
        let output = output_directory("worker-down");
        let store = GenerationStore::open(&output).await.unwrap();
        let snapshot: Arc<StaticSnapshot> = recover_static(static_zip(), "static".into())
            .unwrap()
            .active()
            .await;
        let generated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let feed = encoded_feed(generated_at, &snapshot.version);
        GenerationPublisher::publish(
            &output,
            &store,
            ValidatedGeneration {
                static_snapshot: snapshot,
                generated_at_unix: generated_at,
                source_name: "fixture",
                trip_updates: feed.clone(),
                vehicle_positions: feed.clone(),
                alerts: feed,
                entity_counts: EntityCounts::default(),
            },
        )
        .await
        .unwrap();
        let app = serve::router(serve::AppState::new(
            store,
            crate::config::AccessPolicy::from_allowed_ips_for_test(Default::default()),
            Duration::from_secs(300),
            Arc::new(serve::SystemClock),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = watch::channel(false);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let supervisor = tokio::spawn(supervise_with(
            activity_future(false, Ok(()), Arc::new(AtomicBool::new(false))),
            activity_future(false, Ok(()), Arc::new(AtomicBool::new(false))),
            async move {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .map_err(|_| ServiceError::failed(ServiceActivity::Http))
            },
            Some(WorkerActivity {
                run: worker::run_worker(
                    || tokio::process::Command::new("/nonexistent/browser-worker"),
                    worker::RestartPolicy {
                        initial_backoff: Duration::from_secs(60),
                        ..worker::RestartPolicy::default()
                    },
                    stopped,
                ),
                stop,
            }),
            async {
                let _ = shutdown_rx.await;
            },
        ));
        for path in ["/livez", "/readyz", "/v1/feed-set.json"] {
            let status = tokio::task::spawn_blocking(move || http_status_line(address, path))
                .await
                .unwrap()
                .unwrap();
            assert!(status.starts_with(b"HTTP/1.1 200 "), "{path}");
        }
        shutdown_tx.send(()).unwrap();
        assert_eq!(supervisor.await.unwrap(), Ok(()));
        std::fs::remove_dir_all(output).unwrap();
    }

    /// Counts `fetch` calls, so a cycle that polls twice is visible.
    struct CountingSource(Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait]
    impl RtSource for CountingSource {
        fn name(&self) -> &'static str {
            "counting"
        }
        async fn fetch(&self, _gtfs: &gtfs_structures::Gtfs) -> Result<RtBatch, SourceError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(crate::sources::mock::batch_with(1))
        }
    }

    /// One generation is one upstream poll, even with the train-list writer and every other
    /// decorator around the committer: nothing downstream of the source fetches again.
    #[tokio::test]
    async fn one_cycle_polls_the_source_exactly_once() {
        let output = output_directory("one-poll");
        let store = GenerationStore::open(&output).await.unwrap();
        let snapshots = recover_static(static_zip(), "static".into()).unwrap();
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sources: Vec<Box<dyn RtSource>> = vec![Box::new(CountingSource(polls.clone()))];
        let committer = fleet_trains::WithFleetTrainsFile::new(
            StoreGenerationCommitter::new(output.clone(), store.clone()),
            Some(output.join("tracks").join("fleet-trains.json")),
        );
        let now = SystemTime::now();
        for cycle in 1..=3 {
            // Whether the fixture batch validates is not the point; the poll count is.
            let _ =
                orchestrator::refresh_generation_once(&sources, &snapshots, &committer, now).await;
            assert_eq!(polls.load(Ordering::SeqCst), cycle, "one poll per cycle");
        }
        std::fs::remove_dir_all(output).unwrap();
    }

    struct RefuseToPublish;

    #[async_trait]
    impl GenerationCommitter for RefuseToPublish {
        async fn publish(
            &self,
            _candidate: ValidatedGeneration,
        ) -> Result<Arc<crate::writer::PublishedGeneration>, crate::writer::PublishError> {
            Err(crate::writer::PublishError("fixture refusal".into()))
        }
    }

    /// The worker's train list follows published generations only, and replaces one file.
    #[tokio::test]
    async fn the_fleet_train_list_is_replaced_after_each_published_generation() {
        let output = output_directory("fleet-trains");
        let store = GenerationStore::open(&output).await.unwrap();
        let snapshot: Arc<StaticSnapshot> = recover_static(static_zip(), "static".into())
            .unwrap()
            .active()
            .await;
        let list = output.join("tracks").join("fleet-trains.json");
        let candidate = |generated_at: u64| {
            let feed = encoded_feed(generated_at, &snapshot.version);
            ValidatedGeneration {
                static_snapshot: snapshot.clone(),
                generated_at_unix: generated_at,
                source_name: "fixture",
                trip_updates: feed.clone(),
                vehicle_positions: feed.clone(),
                alerts: feed,
                entity_counts: EntityCounts::default(),
            }
        };

        let refused = fleet_trains::WithFleetTrainsFile::new(RefuseToPublish, Some(list.clone()));
        assert!(refused.publish(candidate(1_786_588_800)).await.is_err());
        assert!(
            !list.exists(),
            "a refused generation must not move the list"
        );

        let committer = fleet_trains::WithFleetTrainsFile::new(
            StoreGenerationCommitter::new(output.clone(), store.clone()),
            Some(list.clone()),
        );
        committer.publish(candidate(1_786_588_801)).await.unwrap();
        committer.publish(candidate(1_786_588_802)).await.unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&list).unwrap()).unwrap();
        assert_eq!(body["schema"], "amtrak-fleet-trains/v1");
        let names: Vec<String> = std::fs::read_dir(list.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["fleet-trains.json".to_string()]);
        std::fs::remove_dir_all(output).unwrap();
    }

    #[tokio::test]
    async fn restart_recovers_last_good_when_refresh_source_is_unavailable() {
        let output = output_directory("restart");
        let store = GenerationStore::open(&output).await.unwrap();
        let recovered = recover_static(static_zip(), "retained-static".into()).unwrap();
        let snapshot: Arc<StaticSnapshot> = recovered.active().await;
        let generated_at = 1_786_588_800;
        let feed = encoded_feed(generated_at, &snapshot.version);
        let published = GenerationPublisher::publish(
            &output,
            &store,
            ValidatedGeneration {
                static_snapshot: snapshot,
                generated_at_unix: generated_at,
                source_name: "fixture",
                trip_updates: feed.clone(),
                vehicle_positions: feed.clone(),
                alerts: feed,
                entity_counts: EntityCounts::default(),
            },
        )
        .await
        .unwrap();
        let retained_id = published.id.clone();
        drop(store);

        let reopened = GenerationStore::open(&output).await.unwrap();
        let snapshots = initial_snapshots(
            &reopened,
            "http://127.0.0.1:1/unavailable.zip",
            &RejectIfCalled,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(snapshots.active().await.version, "retained-static");

        let sources: Vec<Box<dyn RtSource>> = vec![Box::new(MockSource {
            name: "offline",
            behavior: Behavior::Fail,
        })];
        let committer = StoreGenerationCommitter::new(output.clone(), reopened.clone());
        let outcome = orchestrator::refresh_generation_once(
            &sources,
            &snapshots,
            &committer,
            UNIX_EPOCH + Duration::from_secs(generated_at + 1),
        )
        .await;
        assert_eq!(outcome.stage, RefreshStage::Source);
        assert_eq!(reopened.current().await.unwrap().id, retained_id);

        std::fs::remove_dir_all(output).unwrap();
    }
    /// R6.3: a board that never answers must not slow a generation read.
    #[tokio::test]
    async fn hanging_boards_do_not_delay_generation() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream); // accept and never respond
            }
        });
        let mut config = crate::config::TrackConfig::from_map(|_| None).unwrap();
        config.njt_stations.clear();
        config.hartford_url = Some(format!("http://{address}/board"));
        config.request_timeout = Duration::from_secs(30);
        let store = Arc::new(AssignmentStore::new());
        let token_path = std::env::temp_dir().join(format!("tracks-timing-{}", std::process::id()));
        let refresher = tokio::spawn(run_board_refresher(
            store.clone(),
            config.clone(),
            token_path,
            SharedPlatforms::new(config.platforms.clone(), None),
        ));
        tokio::time::sleep(Duration::from_millis(200)).await;

        let gtfs = gtfs_structures::Gtfs::default();
        let inner = || MockSource {
            name: "amtrak",
            behavior: Behavior::Ok(RtBatch::empty()),
        };
        let disabled = realtime_source(inner(), None, None);
        let started = std::time::Instant::now();
        disabled.fetch(&gtfs).await.unwrap();
        let baseline = started.elapsed();

        let enabled = realtime_source(
            inner(),
            None,
            Some(TrackWiring {
                store,
                table: SharedPlatforms::new(config.platforms.clone(), None),
                max_age: config.max_age,
            }),
        );
        let started = std::time::Instant::now();
        enabled.fetch(&gtfs).await.unwrap();
        let with_tracks = started.elapsed();
        refresher.abort();
        assert!(
            with_tracks < baseline + Duration::from_secs(1),
            "{with_tracks:?}"
        );
        assert_eq!(enabled.name(), "amtrak");
    }

    /// R3.10: without track wiring the source is the inner source, undecorated.
    #[tokio::test]
    async fn disabled_tracks_leave_the_source_undecorated() {
        let batch = RtBatch::empty();
        let source = realtime_source(
            MockSource {
                name: "amtrak",
                behavior: Behavior::Ok(batch.clone()),
            },
            None,
            None,
        );
        let fetched = source
            .fetch(&gtfs_structures::Gtfs::default())
            .await
            .unwrap();
        assert_eq!(fetched.trip_updates, batch.trip_updates);
    }

    #[test]
    fn a_leftover_role_refuses_to_start() {
        for accepted in [None, Some(""), Some("combined"), Some(" combined ")] {
            assert!(check_role(accepted).is_ok(), "{accepted:?}");
        }
        for removed in ["vehicle-positions", "trip-updates", "alerts", "nonsense"] {
            let error = check_role(Some(removed)).unwrap_err();
            assert!(error.to_string().contains("AMTRAK_ROLE"), "{error}");
            assert!(error.to_string().contains("README"), "{error}");
        }
    }

    #[test]
    fn healthcheck_uses_configured_port_on_loopback() {
        assert_eq!(
            healthcheck_address("0.0.0.0:9000").unwrap(),
            "127.0.0.1:9000".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            healthcheck_address("[::1]:7000").unwrap(),
            "127.0.0.1:7000".parse::<SocketAddr>().unwrap()
        );
        assert!(healthcheck_address("not-an-address").is_err());
    }
}
