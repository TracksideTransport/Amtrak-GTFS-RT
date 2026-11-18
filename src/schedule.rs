//! Early track refreshes.
//!
//! The base poll runs on a fixed interval. A feed request can ask for a train or
//! a station sooner. Those hints are debounced, coalesced per train, and capped
//! so a burst cannot exceed the polite per-cycle fetch limit. Recording a hint
//! does not fetch anything: the service queues each flush as one file under
//! `{AMTRAK_OUTPUT_DIR}/tracks/early-refresh/`, which the browser worker's track
//! poller reads and deletes, so a hint is acted on once.

#![deny(missing_docs)]

use crate::config::EarlyRefreshConfig;
use crate::serve::{TripInterest, TripInterestHook};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Default minutes between full track polls.
pub const DEFAULT_BASE_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Quiet period that collapses a burst of hints into one refresh.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_secs(5);

/// Minimum time between refreshes of the same train.
pub const DEFAULT_MIN_GAP: Duration = Duration::from_secs(90);

/// Most trains one early flush may release. Matches the status fetcher's cap.
pub const DEFAULT_MAX_PER_FLUSH: usize = 24;

/// In-memory early-refresh queue.
///
/// # Examples
///
/// ```
/// use amtrak_gtfs_rt_service::schedule::TrackScheduler;
/// use std::time::{Duration, Instant};
///
/// let mut scheduler = TrackScheduler::new(
///     Duration::from_secs(900),
///     Duration::from_secs(5),
///     Duration::from_secs(90),
///     24,
/// );
/// let start = Instant::now();
/// scheduler.enqueue_train("167", start);
/// scheduler.enqueue_train("167", start + Duration::from_secs(1));
/// assert!(scheduler.take_early(start + Duration::from_secs(5)).is_empty());
/// assert_eq!(
///     scheduler.take_early(start + Duration::from_secs(6)),
///     vec!["167".to_string()]
/// );
/// ```
#[derive(Clone, Debug)]
pub struct TrackScheduler {
    base_interval: Duration,
    debounce: Duration,
    min_gap: Duration,
    max_per_flush: usize,
    last_base: Option<Instant>,
    /// Train number -> last time a caller asked for it in the current burst.
    last_touch: HashMap<String, Instant>,
    last_refresh: HashMap<String, Instant>,
    station_touch: HashMap<String, Instant>,
}

impl TrackScheduler {
    /// Builds a scheduler.
    ///
    /// # Arguments
    ///
    /// * `base_interval` - Time between full track polls.
    /// * `debounce` - Quiet time after the latest hint before that train is due.
    /// * `min_gap` - Minimum time between two refreshes of one train.
    /// * `max_per_flush` - Most trains `take_early` returns at once.
    ///
    /// # Panics
    ///
    /// Never panics.
    ///
    /// # Examples
    ///
    /// ```
    /// use amtrak_gtfs_rt_service::schedule::{TrackScheduler, DEFAULT_MAX_PER_FLUSH};
    /// use std::time::Duration;
    ///
    /// let scheduler = TrackScheduler::new(
    ///     Duration::from_secs(900),
    ///     Duration::from_secs(5),
    ///     Duration::from_secs(90),
    ///     DEFAULT_MAX_PER_FLUSH,
    /// );
    /// assert_eq!(scheduler.max_per_flush(), 24);
    /// ```
    pub fn new(
        base_interval: Duration,
        debounce: Duration,
        min_gap: Duration,
        max_per_flush: usize,
    ) -> Self {
        Self {
            base_interval,
            debounce,
            min_gap,
            max_per_flush,
            last_base: None,
            last_touch: HashMap::new(),
            last_refresh: HashMap::new(),
            station_touch: HashMap::new(),
        }
    }

    /// The cap applied to one early flush.
    ///
    /// # Returns
    ///
    /// The maximum number of trains `take_early` will release.
    pub fn max_per_flush(&self) -> usize {
        self.max_per_flush
    }

    /// Records interest in one train. Later touches of the same train coalesce.
    ///
    /// # Arguments
    ///
    /// * `train` - Train number. Blank input is ignored. Leading zeros are kept.
    /// * `now` - Time of the request. This method does not sleep or fetch.
    pub fn enqueue_train(&mut self, train: &str, now: Instant) {
        let Some(train) = normalize_train(train) else {
            return;
        };
        self.last_touch.insert(train, now);
    }

    /// Records a station code. Expansion to trains happens in the track poller.
    ///
    /// # Arguments
    ///
    /// * `station` - Amtrak stop code.
    /// * `now` - Time of the request.
    pub fn enqueue_station(&mut self, station: &str, now: Instant) {
        let station = station.trim().to_uppercase();
        if station.len() >= 3 && station.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            self.station_touch.insert(station, now);
        }
    }

    /// Station hints whose debounce has elapsed.
    ///
    /// # Arguments
    ///
    /// * `now` - Time the flusher runs.
    ///
    /// # Returns
    ///
    /// Station codes to expand into trains.
    pub fn take_stations(&mut self, now: Instant) -> Vec<String> {
        let due: Vec<String> = self
            .station_touch
            .iter()
            .filter(|(_, touched)| now.saturating_duration_since(**touched) >= self.debounce)
            .map(|(station, _)| station.clone())
            .collect();
        for station in &due {
            self.station_touch.remove(station);
        }
        due
    }

    /// Records every train number in `trains`.
    ///
    /// # Arguments
    ///
    /// * `trains` - Train numbers from a station hint that was already expanded.
    /// * `now` - Time of the request.
    pub fn enqueue_trains<I, S>(&mut self, trains: I, now: Instant)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for train in trains {
            self.enqueue_train(train.as_ref(), now);
        }
    }

    /// Trains whose latest hint has been quiet for the debounce and whose last
    /// refresh is at least `min_gap` old.
    ///
    /// At most `max_per_flush` trains are returned, lowest number first. The
    /// rest stay queued. A train that is still inside `min_gap` stays queued
    /// without extending its debounce.
    ///
    /// # Arguments
    ///
    /// * `now` - Time the background flusher runs.
    ///
    /// # Returns
    ///
    /// Train numbers to refresh now.
    pub fn take_early(&mut self, now: Instant) -> Vec<String> {
        let mut ready: Vec<String> = self
            .last_touch
            .iter()
            .filter(|(train, touched)| {
                now.saturating_duration_since(**touched) >= self.debounce
                    && self.last_refresh.get(*train).is_none_or(|previous| {
                        now.saturating_duration_since(*previous) >= self.min_gap
                    })
            })
            .map(|(train, _)| train.clone())
            .collect();
        ready.sort();
        let count = ready.len().min(self.max_per_flush);
        let due = ready.drain(..count).collect::<Vec<_>>();
        for train in &due {
            self.last_touch.remove(train);
            self.last_refresh.insert(train.clone(), now);
        }
        due
    }

    /// Whether a full base poll is due.
    ///
    /// The first call is due immediately so a process does not wait out the
    /// whole interval before its first pass. Later calls are due once
    /// `base_interval` has elapsed since the previous due call.
    ///
    /// # Arguments
    ///
    /// * `now` - Time the scheduler is asked.
    ///
    /// # Returns
    ///
    /// `true` when the caller should run a full track poll.
    pub fn base_due(&mut self, now: Instant) -> bool {
        match self.last_base {
            Some(previous) if now.saturating_duration_since(previous) < self.base_interval => false,
            _ => {
                self.last_base = Some(now);
                true
            }
        }
    }
}

fn normalize_train(train: &str) -> Option<String> {
    let train = train.trim();
    if train.is_empty() || !train.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(train.to_string())
}

impl TrackScheduler {
    /// A scheduler with the parsed `AMTRAK_TRACKS_*` tuning.
    pub fn from_config(config: &EarlyRefreshConfig) -> Self {
        Self::new(
            config.base_interval,
            config.debounce,
            config.min_gap,
            config.max_per_flush,
        )
    }
}

/// The HTTP layer's trip-interest hook: records each `train` and `station` query in `scheduler`.
/// It only takes a lock and returns; the response never waits on a refresh.
pub fn trip_interest_hook(scheduler: Arc<Mutex<TrackScheduler>>) -> TripInterestHook {
    Arc::new(move |interest: TripInterest| {
        let Ok(mut guard) = scheduler.lock() else {
            return;
        };
        let now = Instant::now();
        guard.enqueue_trains(&interest.trains, now);
        for station in &interest.stations {
            guard.enqueue_station(station, now);
        }
    })
}

/// Once a second, writes whatever `scheduler` releases as one early-refresh hint file.
pub async fn run_early_hint_writer(scheduler: Arc<Mutex<TrackScheduler>>, output_dir: PathBuf) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (trains, stations) = {
            let Ok(mut guard) = scheduler.lock() else {
                continue;
            };
            let now = Instant::now();
            (guard.take_early(now), guard.take_stations(now))
        };
        if trains.is_empty() && stations.is_empty() {
            continue;
        }
        if let Err(error) = write_early_hint(&output_dir, &trains, &stations) {
            tracing::warn!(%error, "early track-refresh hint not written");
        }
    }
}

/// Queues one batch of early track-refresh hints for the worker's track poller.
///
/// Each flush is its own file under `tracks/early-refresh/`, written to a dot-prefixed
/// temporary name and renamed into place. The poller reads and deletes every queued
/// file, so a hint is acted on once and a later flush never overwrites an unread one.
///
/// # Errors
///
/// Returns an I/O error when the queue directory or file cannot be written.
pub fn write_early_hint(
    output_dir: &Path,
    trains: &[String],
    stations: &[String],
) -> std::io::Result<PathBuf> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let queue = output_dir.join("tracks").join("early-refresh");
    std::fs::create_dir_all(&queue)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let name = format!("{nanos}-{}.json", SEQUENCE.fetch_add(1, Ordering::Relaxed));
    let body = serde_json::json!({"trains": trains, "stations": stations}).to_string();
    let temporary = queue.join(format!(".{name}"));
    std::fs::write(&temporary, body)?;
    let path = queue.join(name);
    std::fs::rename(&temporary, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn early_hints_queue_one_file_per_flush_without_temporaries() {
        let dir = std::env::temp_dir().join(format!("early-hints-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = write_early_hint(&dir, &["163".into()], &[]).unwrap();
        let second = write_early_hint(&dir, &[], &["NYP".into()]).unwrap();
        assert_ne!(first, second);
        let mut names: Vec<String> = std::fs::read_dir(dir.join("tracks/early-refresh"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().all(|name| !name.starts_with('.')));
        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&second).unwrap()).unwrap();
        assert_eq!(body, serde_json::json!({"trains": [], "stations": ["NYP"]}));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_trip_interest_hook_only_records_requests() {
        let config = EarlyRefreshConfig::from_map(|_| None).unwrap();
        let scheduler = Arc::new(Mutex::new(TrackScheduler::from_config(&config)));
        let hook = trip_interest_hook(scheduler.clone());
        hook(TripInterest {
            trains: vec!["167".into()],
            stations: vec!["NYP".into()],
        });
        let later = Instant::now() + config.debounce + Duration::from_secs(1);
        let mut guard = scheduler.lock().unwrap();
        assert_eq!(guard.take_early(later), vec!["167".to_string()]);
        assert_eq!(guard.take_stations(later), vec!["NYP".to_string()]);
    }

    fn scheduler() -> TrackScheduler {
        TrackScheduler::new(
            Duration::from_secs(900),
            Duration::from_secs(5),
            Duration::from_secs(90),
            24,
        )
    }

    #[test]
    fn debounce_coalesces_one_train_and_keeps_distinct_trains() {
        let mut scheduler = scheduler();
        let start = Instant::now();
        scheduler.enqueue_train("167", start);
        scheduler.enqueue_train(" 167 ", start + Duration::from_secs(1));
        scheduler.enqueue_train("66", start + Duration::from_secs(2));
        assert!(scheduler
            .take_early(start + Duration::from_secs(4))
            .is_empty());
        assert_eq!(
            scheduler.take_early(start + Duration::from_secs(7)),
            vec!["167".to_string(), "66".to_string()]
        );
    }

    #[test]
    fn min_gap_holds_a_train_and_the_cap_leaves_the_rest_queued() {
        let mut scheduler = TrackScheduler::new(
            Duration::from_secs(900),
            Duration::from_secs(0),
            Duration::from_secs(90),
            2,
        );
        let start = Instant::now();
        for train in ["10", "2", "30", "4"] {
            scheduler.enqueue_train(train, start);
        }
        assert_eq!(
            scheduler.take_early(start),
            vec!["10".to_string(), "2".to_string()]
        );
        assert_eq!(
            scheduler.take_early(start + Duration::from_secs(1)),
            vec!["30".to_string(), "4".to_string()]
        );
        scheduler.enqueue_train("2", start + Duration::from_secs(2));
        assert!(scheduler
            .take_early(start + Duration::from_secs(3))
            .is_empty());
        assert_eq!(
            scheduler.take_early(start + Duration::from_secs(92)),
            vec!["2".to_string()]
        );
    }

    #[test]
    fn base_interval_gates_the_full_poll() {
        let mut scheduler = scheduler();
        let start = Instant::now();
        assert!(scheduler.base_due(start));
        assert!(!scheduler.base_due(start + Duration::from_secs(899)));
        assert!(scheduler.base_due(start + Duration::from_secs(900)));
    }

    #[test]
    fn station_hint_coalesces_with_a_direct_train_hint() {
        let mut scheduler = scheduler();
        let start = Instant::now();
        scheduler.enqueue_trains(["167", "167", ""], start);
        scheduler.enqueue_train("167", start);
        assert_eq!(
            scheduler.take_early(start + Duration::from_secs(5)),
            vec!["167".to_string()]
        );
    }
}
