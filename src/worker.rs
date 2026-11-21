//! Starts, restarts, and stops the built-in browser worker (`python -m worker`).
//!
//! Invariants:
//!
//! - The worker runs only when a browser source is on and reads the built-in worker's URL. With
//!   `AMTRAK_TRACKS_AMTRAK` and `AMTRAK_ADVISORIES` both off, no Python or Chromium process starts.
//! - The worker always listens on `127.0.0.1`, whatever the inherited environment says.
//! - Nothing in the request path waits on the worker. Its sources fail open in the service, so
//!   this module only keeps the process alive.
//! - A worker exit is restarted with exponential backoff. After [`RestartPolicy::max_failures`]
//!   consecutive exits the supervisor gives up and the service exits non-zero, so the container
//!   restart policy takes over. A run that lasted [`RestartPolicy::stable_after`] resets the count.
//! - The worker runs in its own process group, and the whole group is killed after each exit.
//!   Playwright may start Chromium in a group of its own; that browser exits when its pipe to the
//!   killed driver closes, and the worker clears leftovers when it starts again.
//! - The worker gets the service's output directory as an absolute path and never the NJ Transit
//!   RailData credentials, which only the service uses.
//! - Stopping sends `SIGTERM`, waits [`RestartPolicy::grace`], then kills the group.

use crate::config::{is_worker_url, AdvisoryConfig, TrackConfig, WORKER_BASE_URL};
use crate::{ServiceActivity, ServiceError};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::time::Instant;

/// Directory holding the worker package (`worker/`) in the container image.
pub const DEFAULT_WORKER_ROOT: &str = "/app";
/// Host the worker binds. Never configurable: the worker is not a public service.
const WORKER_LISTEN_HOST: &str = "127.0.0.1";

/// `path` as an absolute path string. The worker runs from its package root, so a relative
/// path such as the host default `./out` would name a different directory there.
fn absolute(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

/// Which browser sources the embedded worker runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerPlan {
    /// Amtrak's track and gate status documents.
    pub tracks: bool,
    /// The service-notices page.
    pub notices: bool,
    /// Directory that contains the `worker` package.
    pub root: PathBuf,
    /// The train list the service writes after each generation (`AMTRAK_FLEET_TRAINS_FILE`), so
    /// the track poller never fetches `getTrainsData` itself.
    pub fleet_trains_file: Option<PathBuf>,
    /// The service's output directory (`AMTRAK_OUTPUT_DIR`): the notices snapshots and the
    /// early-refresh queue live under it.
    pub output_dir: Option<PathBuf>,
    /// How often the worker refetches Amtrak's station directory, passed as
    /// `STATIONS_REFRESH_SECS`: the service's `AMTRAK_STOP_NAMES_REFRESH_SECS`.
    pub stations_refresh: Option<Duration>,
}

impl WorkerPlan {
    /// The worker to start, or `None` when no browser source needs the built-in worker or the
    /// worker package is not installed (`cargo run` on a host).
    ///
    /// A source whose URL was overridden to point elsewhere does not need the built-in worker.
    pub fn from_configs(
        tracks: &TrackConfig,
        advisories: &AdvisoryConfig,
        root: &Path,
    ) -> Option<WorkerPlan> {
        let plan = WorkerPlan::wanted(tracks, advisories, root)?;
        root.join("worker").is_dir().then_some(plan)
    }

    /// Like [`WorkerPlan::from_configs`] without checking that the package is installed.
    pub fn wanted(
        tracks: &TrackConfig,
        advisories: &AdvisoryConfig,
        root: &Path,
    ) -> Option<WorkerPlan> {
        let track_source = tracks.enabled
            && tracks.amtrak_enabled
            && tracks.amtrak_url.as_deref().is_some_and(is_worker_url);
        let notices = advisories.enabled && is_worker_url(&advisories.url);
        (track_source || notices).then(|| WorkerPlan {
            tracks: track_source,
            notices,
            root: root.to_path_buf(),
            fleet_trains_file: None,
            output_dir: None,
            stations_refresh: None,
        })
    }

    /// Points the track poller at the service's train list.
    pub fn with_fleet_trains_file(mut self, path: PathBuf) -> Self {
        self.fleet_trains_file = Some(path);
        self
    }

    /// Shares the service's output directory with the worker.
    pub fn with_output_dir(mut self, path: PathBuf) -> Self {
        self.output_dir = Some(path);
        self
    }

    /// Has the track source refetch the station directory every `every` (whole seconds), unless the
    /// plan runs no track source.
    pub fn with_stations_refresh(mut self, every: Duration) -> Self {
        self.stations_refresh = Some(every);
        self
    }

    /// Environment the worker gets on top of the service's own. The worker reads the same
    /// `AMTRAK_*` flags, so a source this plan does not run is turned off explicitly.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        let port = WORKER_BASE_URL
            .rsplit(':')
            .next()
            .unwrap_or("8081")
            .to_string();
        let mut env = vec![
            ("LISTEN_HOST", WORKER_LISTEN_HOST.to_string()),
            ("LISTEN_PORT", port),
        ];
        if !self.tracks {
            env.push(("AMTRAK_TRACKS_AMTRAK", "off".to_string()));
        }
        if let (true, Some(path)) = (self.tracks, &self.fleet_trains_file) {
            env.push(("AMTRAK_FLEET_TRAINS_FILE", absolute(path)));
        }
        if let Some(path) = &self.output_dir {
            env.push(("AMTRAK_OUTPUT_DIR", absolute(path)));
        }
        if let (true, Some(every)) = (self.tracks, self.stations_refresh) {
            env.push(("STATIONS_REFRESH_SECS", every.as_secs().to_string()));
        }
        if !self.notices {
            env.push(("AMTRAK_ADVISORIES", "off".to_string()));
        }
        env
    }

    /// The `python3 -m worker` command for this plan.
    pub fn command(&self) -> Command {
        let mut command = Command::new("python3");
        command
            .args(["-m", "worker"])
            .current_dir(&self.root)
            .envs(self.env())
            .env_remove("NJT_RAILDATA_USERNAME")
            .env_remove("NJT_RAILDATA_PASSWORD")
            .stdin(std::process::Stdio::null());
        command
    }

    /// Names of the sources this worker runs, for logs.
    pub fn describe(&self) -> &'static str {
        match (self.tracks, self.notices) {
            (true, true) => "tracks and notices",
            (true, false) => "tracks",
            (false, true) => "notices",
            (false, false) => "nothing",
        }
    }
}

/// Restart and shutdown timing for the worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestartPolicy {
    /// Consecutive exits after which the supervisor gives up.
    pub max_failures: u32,
    /// Delay before the first restart. It doubles with each consecutive exit.
    pub initial_backoff: Duration,
    /// Longest delay between restarts.
    pub max_backoff: Duration,
    /// A run at least this long resets the consecutive-exit count.
    pub stable_after: Duration,
    /// Time between `SIGTERM` and `SIGKILL` when stopping.
    pub grace: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_failures: 5,
            initial_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(60),
            stable_after: Duration::from_secs(300),
            grace: Duration::from_secs(5),
        }
    }
}

impl RestartPolicy {
    /// Delay before restarting after the `failures`th consecutive exit (1-based).
    pub fn backoff(&self, failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(16);
        self.initial_backoff
            .saturating_mul(1 << doublings)
            .min(self.max_backoff)
    }
}

/// Runs the worker until `stop` turns `true` (or its sender is dropped).
///
/// `command` builds a fresh command for each start.
///
/// # Errors
///
/// Returns a closed [`ServiceError`] for [`ServiceActivity::BrowserWorker`] once the worker has
/// exited [`RestartPolicy::max_failures`] times in a row.
pub async fn run_worker<F>(
    mut command: F,
    policy: RestartPolicy,
    mut stop: watch::Receiver<bool>,
) -> Result<(), ServiceError>
where
    F: FnMut() -> Command,
{
    let mut failures: u32 = 0;
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let started = Instant::now();
        let mut command = command();
        command.kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        match command.spawn() {
            Ok(mut child) => {
                // Read before waiting: `Child::id` is `None` once the child has been reaped.
                let group = child.id();
                tracing::info!(pid = group, "browser worker started");
                tokio::select! {
                    status = child.wait() => {
                        kill_group(group, true);
                        match status {
                            Ok(status) => tracing::warn!(%status, "browser worker exited"),
                            Err(error) => tracing::warn!(%error, "browser worker wait failed"),
                        }
                        if started.elapsed() >= policy.stable_after {
                            failures = 0;
                        }
                    }
                    _ = stop.changed() => {
                        terminate(&mut child, group, policy.grace).await;
                        return Ok(());
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "browser worker did not start"),
        }
        failures += 1;
        if failures >= policy.max_failures {
            tracing::error!(
                failures,
                "browser worker exited too many times in a row; stopping the service"
            );
            return Err(ServiceError::failed(ServiceActivity::BrowserWorker));
        }
        let delay = policy.backoff(failures);
        tracing::info!(
            failures,
            delay_secs = delay.as_secs_f64(),
            "restarting the browser worker"
        );
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            _ = stop.changed() => return Ok(()),
        }
    }
}

/// `SIGTERM` the worker's process group, wait up to `grace`, then `SIGKILL` it.
async fn terminate(child: &mut Child, group: Option<u32>, grace: Duration) {
    kill_group(group, false);
    if tokio::time::timeout(grace, child.wait()).await.is_err() {
        tracing::warn!("browser worker ignored SIGTERM; killing it");
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    kill_group(group, true);
}

/// Signals the worker's process group. The group id is the worker's pid, which stays reserved
/// while any member (a Chromium it left behind) is alive.
#[cfg(unix)]
fn kill_group(group: Option<u32>, force: bool) {
    use rustix::process::{kill_process_group, Pid, Signal};
    let Some(pid) = group
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw)
    else {
        return;
    };
    let signal = if force { Signal::KILL } else { Signal::TERM };
    // ESRCH (nothing left in the group) is the normal case after an exit.
    let _ = kill_process_group(pid, signal);
}

#[cfg(not(unix))]
fn kill_group(_group: Option<u32>, _force: bool) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn fast_policy(max_failures: u32) -> RestartPolicy {
        RestartPolicy {
            max_failures,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(40),
            stable_after: Duration::from_secs(60),
            grace: Duration::from_millis(500),
        }
    }

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    fn configs(pairs: &[(&str, &str)]) -> (TrackConfig, AdvisoryConfig) {
        let get = |key: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_string())
        };
        (
            TrackConfig::from_map(get).unwrap(),
            AdvisoryConfig::from_map(get).unwrap(),
        )
    }

    #[test]
    fn plan_runs_only_the_browser_sources_that_read_the_built_in_worker() {
        let root = Path::new("/app");
        let (tracks, advisories) = configs(&[]);
        let both = WorkerPlan::wanted(&tracks, &advisories, root).unwrap();
        assert!(both.tracks && both.notices);
        assert_eq!(
            both.env(),
            vec![
                ("LISTEN_HOST", "127.0.0.1".to_string()),
                ("LISTEN_PORT", "8081".to_string())
            ]
        );

        let (tracks, advisories) = configs(&[("AMTRAK_ADVISORIES", "off")]);
        let tracks_only = WorkerPlan::wanted(&tracks, &advisories, root).unwrap();
        assert!(tracks_only.tracks && !tracks_only.notices);
        assert!(tracks_only
            .env()
            .contains(&("AMTRAK_ADVISORIES", "off".to_string())));
        let with_list = tracks_only
            .clone()
            .with_fleet_trains_file(PathBuf::from("/data/tracks/f.json"));
        assert!(with_list.env().contains(&(
            "AMTRAK_FLEET_TRAINS_FILE",
            "/data/tracks/f.json".to_string()
        )));

        // The service's refresh interval reaches the worker in whole seconds, and only if given.
        let week = Duration::from_secs(604_800);
        assert!(tracks_only
            .clone()
            .with_stations_refresh(week)
            .env()
            .contains(&("STATIONS_REFRESH_SECS", "604800".to_string())));
        assert!(!tracks_only
            .env()
            .iter()
            .any(|(key, _)| *key == "STATIONS_REFRESH_SECS"));

        // Tracks on but Amtrak's own documents off: only notices need a browser.
        let (tracks, advisories) = configs(&[("AMTRAK_TRACKS_AMTRAK", "off")]);
        let notices_only = WorkerPlan::wanted(&tracks, &advisories, root).unwrap();
        assert!(!notices_only.tracks && notices_only.notices);
        assert!(notices_only
            .env()
            .contains(&("AMTRAK_TRACKS_AMTRAK", "off".to_string())));
        // No track poller, so no train list either.
        assert!(!notices_only
            .with_fleet_trains_file(PathBuf::from("/data/tracks/f.json"))
            .env()
            .iter()
            .any(|(key, _)| *key == "AMTRAK_FLEET_TRAINS_FILE"));

        for off in [
            vec![
                ("AMTRAK_TRACKS_AMTRAK", "off"),
                ("AMTRAK_ADVISORIES", "off"),
            ],
            vec![("AMTRAK_TRACKS", "off"), ("AMTRAK_ADVISORIES", "off")],
            vec![
                (
                    "AMTRAK_TRACKS_AMTRAK_URL",
                    "http://worker.internal:8081/tracks.json",
                ),
                ("AMTRAK_ADVISORIES_URL", ""),
            ],
        ] {
            let (tracks, advisories) = configs(&off);
            assert_eq!(
                WorkerPlan::wanted(&tracks, &advisories, root),
                None,
                "{off:?}"
            );
        }
    }

    #[test]
    fn worker_gets_absolute_paths_and_no_raildata_credentials() {
        let (tracks, advisories) = configs(&[]);
        let plan = WorkerPlan::wanted(&tracks, &advisories, Path::new("/app"))
            .unwrap()
            .with_fleet_trains_file(PathBuf::from("out/tracks/fleet-trains.json"))
            .with_output_dir(PathBuf::from("out"));
        let env = plan.env();
        let cwd = std::env::current_dir().unwrap();
        assert!(env.contains(&("AMTRAK_OUTPUT_DIR", cwd.join("out").display().to_string())));
        assert!(env.contains(&(
            "AMTRAK_FLEET_TRAINS_FILE",
            cwd.join("out/tracks/fleet-trains.json")
                .display()
                .to_string()
        )));
        let command = plan.command();
        let removed: Vec<_> = command
            .as_std()
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect();
        assert!(removed.contains(&"NJT_RAILDATA_USERNAME".to_string()));
        assert!(removed.contains(&"NJT_RAILDATA_PASSWORD".to_string()));
    }

    #[test]
    fn plan_needs_the_installed_worker_package() {
        let (tracks, advisories) = configs(&[]);
        let missing = std::env::temp_dir().join(format!("no-worker-{}", std::process::id()));
        assert_eq!(
            WorkerPlan::from_configs(&tracks, &advisories, &missing),
            None
        );
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let policy = RestartPolicy::default();
        let delays: Vec<u64> = (1..=7).map(|n| policy.backoff(n).as_secs()).collect();
        assert_eq!(delays, vec![2, 4, 8, 16, 32, 60, 60]);
    }

    #[tokio::test]
    async fn repeated_exits_exhaust_the_budget_and_fail_the_service() {
        let (_stop_tx, stop_rx) = watch::channel(false);
        let mut starts = 0;
        let result = run_worker(
            || {
                starts += 1;
                shell("exit 3")
            },
            fast_policy(3),
            stop_rx,
        )
        .await;
        assert_eq!(
            result,
            Err(ServiceError::failed(ServiceActivity::BrowserWorker))
        );
        assert_eq!(starts, 3);
    }

    #[tokio::test]
    async fn a_worker_that_will_not_start_counts_as_a_failure() {
        let (_stop_tx, stop_rx) = watch::channel(false);
        let result = run_worker(
            || Command::new("/nonexistent/browser-worker"),
            fast_policy(2),
            stop_rx,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn an_exited_worker_is_restarted() {
        let dir = std::env::temp_dir().join(format!("worker-restart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("starts");
        let (stop_tx, stop_rx) = watch::channel(false);
        let script = format!("echo x >> '{}'; exit 1", marker.display());
        let task = tokio::spawn(run_worker(
            move || shell(&script),
            fast_policy(100),
            stop_rx,
        ));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let starts = std::fs::read_to_string(&marker)
                .map(|text| text.lines().count())
                .unwrap_or(0);
            if starts >= 3 {
                break;
            }
            assert!(Instant::now() < deadline, "worker was not restarted");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        stop_tx.send(true).unwrap();
        assert_eq!(task.await.unwrap(), Ok(()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stop_terminates_the_worker_and_its_children_within_the_grace_period() {
        let dir = std::env::temp_dir().join(format!("worker-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pids = dir.join("pids");
        // The "worker" ignores SIGTERM and leaves a grandchild, like a stuck driver and Chromium.
        let script = format!(
            "trap '' TERM; sleep 30 & echo $! > '{grand}'; echo $$ > '{own}'; wait",
            grand = pids.with_extension("grandchild").display(),
            own = pids.with_extension("worker").display(),
        );
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(run_worker(move || shell(&script), fast_policy(5), stop_rx));
        let grandchild_file = pids.with_extension("grandchild");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !grandchild_file.exists() {
            assert!(Instant::now() < deadline, "worker never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let grandchild: i32 = std::fs::read_to_string(&grandchild_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        let stopping = Instant::now();
        stop_tx.send(true).unwrap();
        assert_eq!(task.await.unwrap(), Ok(()));
        assert!(stopping.elapsed() < Duration::from_secs(3));

        let alive = |pid: i32| {
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_ok()
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while alive(grandchild) {
            assert!(Instant::now() < deadline, "the worker's child outlived it");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stop_during_backoff_returns_promptly() {
        let (stop_tx, stop_rx) = watch::channel(false);
        let policy = RestartPolicy {
            initial_backoff: Duration::from_secs(30),
            ..fast_policy(10)
        };
        let task = tokio::spawn(run_worker(|| shell("exit 1"), policy, stop_rx));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let stopping = Instant::now();
        stop_tx.send(true).unwrap();
        assert_eq!(task.await.unwrap(), Ok(()));
        assert!(stopping.elapsed() < Duration::from_secs(2));
    }
}
