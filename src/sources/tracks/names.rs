//! Renames stops from the worker's station names.
//!
//! The browser worker keeps Amtrak's station directory and the names seen in train-status
//! documents, and serves them at `/stations.json`. This task reads that document, and when a
//! renaming is due, applies it through [`SharedNames`], which persists it and wakes the static
//! refresh. A failed or empty read never replaces the applied names.

use crate::static_names::{AppliedNames, DirectoryStation, SeenStation, SharedNames, StationNames};
use chrono::{DateTime, NaiveDateTime, Utc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The first renaming waits this long after the directory's first successful fetch.
const FIRST_RENAMING_DELAY: Duration = Duration::from_secs(86_400);
/// How often the task checks whether a renaming is due.
const CHECK_EVERY: Duration = Duration::from_secs(3_600);

/// Converts the worker's `/stations.json` into names, plus when the directory was fetched.
///
/// Directory rows without a 3-letter `code`, a `name` or a `city` are skipped. Seen entries carry
/// `name`, `displayName`, `facility` and `audioName`.
///
/// # Errors
///
/// Returns a message when the body is not a JSON object.
pub fn parse_stations(body: &str) -> Result<(StationNames, Option<SystemTime>), String> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    let object = value
        .as_object()
        .ok_or("stations document is not an object")?;
    let text = |row: &serde_json::Value, key: &str| {
        row.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    let code = |raw: &str| {
        let raw = raw.trim();
        (raw.len() == 3 && raw.chars().all(|c| c.is_ascii_alphabetic()))
            .then(|| raw.to_ascii_uppercase())
    };
    let mut names = StationNames::default();
    for row in object
        .get("directory")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(code), Some(name), Some(city)) = (
            text(row, "code").and_then(|raw| code(&raw)),
            text(row, "name"),
            text(row, "city"),
        ) else {
            continue;
        };
        names.directory.insert(
            code,
            DirectoryStation {
                name,
                city,
                facility: text(row, "facilityName64"),
            },
        );
    }
    for (key, row) in object
        .get("seen")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
    {
        let Some(code) = code(key) else { continue };
        names.seen.insert(
            code,
            SeenStation {
                name: text(row, "name"),
                display_name: text(row, "displayName"),
                facility: text(row, "facility"),
                audio_name: text(row, "audioName"),
            },
        );
    }
    let fetched_at = object
        .get("directory_fetched_at")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_instant);
    Ok((names, fetched_at))
}

/// An ISO-8601 instant; a value without an offset is read as UTC.
fn parse_instant(text: &str) -> Option<SystemTime> {
    let text = text.trim();
    let utc = DateTime::parse_from_rfc3339(text)
        .map(|at| at.with_timezone(&Utc))
        .or_else(|_| {
            NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").map(|at| at.and_utc())
        })
        .ok()?;
    let seconds = u64::try_from(utc.timestamp()).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(seconds))
}

/// Whether a renaming is due at `now`.
///
/// With nothing applied it is due 24 hours after the directory's first fetch (never, when there is
/// no directory). Otherwise it is due `every` after the last application.
pub fn renaming_due(
    applied: Option<&AppliedNames>,
    directory_fetched_at: Option<SystemTime>,
    now: SystemTime,
    every: Duration,
) -> bool {
    match applied {
        None => directory_fetched_at.is_some_and(|at| at + FIRST_RENAMING_DELAY <= now),
        // An overflowing due time (a huge persisted value or interval) means never.
        Some(applied) => UNIX_EPOCH
            .checked_add(Duration::from_secs(applied.applied_at_unix))
            .and_then(|at| at.checked_add(every))
            .is_some_and(|due| due <= now),
    }
}

/// What one check did.
#[derive(Debug, Eq, PartialEq)]
pub enum NamesOutcome {
    /// Not due; nothing was requested.
    NotDue,
    /// The read failed, was unusable or empty, or the directory is too young; names are kept.
    Kept,
    /// New names were applied.
    Applied,
}

/// One check: when a renaming is due, reads, parses and applies the worker's names.
pub async fn refresh_names_once(
    http: &reqwest::Client,
    url: &str,
    names: &SharedNames,
    every: Duration,
    now: SystemTime,
) -> NamesOutcome {
    let applied = names.applied();
    // With names applied the due time is known without asking the worker.
    if applied.is_some() && !renaming_due(applied.as_deref(), None, now, every) {
        return NamesOutcome::NotDue;
    }
    let body = match read_stations(http, url).await {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(%error, "station names read failed; keeping the applied names");
            return NamesOutcome::Kept;
        }
    };
    let (inputs, fetched_at) = match parse_stations(&body) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(%error, "station names unreadable; keeping the applied names");
            return NamesOutcome::Kept;
        }
    };
    if inputs.directory.is_empty() && inputs.seen.is_empty() {
        tracing::warn!("station names are empty; keeping the applied names");
        return NamesOutcome::Kept;
    }
    // An empty directory never replaces applied names (a reset worker or a blocked fetch).
    if applied.is_some() && inputs.directory.is_empty() && fetched_at.is_none() {
        tracing::warn!("station directory is empty; keeping the applied names");
        return NamesOutcome::Kept;
    }
    if !renaming_due(applied.as_deref(), fetched_at, now, every) {
        return NamesOutcome::Kept;
    }
    let applied_at_unix = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    match names.set_applied(AppliedNames {
        inputs,
        applied_at_unix,
    }) {
        Ok(()) => {
            tracing::info!("station names applied");
            NamesOutcome::Applied
        }
        Err(error) => {
            tracing::warn!(%error, "station names could not be saved; keeping the applied names");
            NamesOutcome::Kept
        }
    }
}

async fn read_stations(http: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = http
        .get(url)
        .send()
        .await
        .map_err(|error| format!("request failed: {}", error.without_url()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("status {status}"));
    }
    response
        .text()
        .await
        .map_err(|error| format!("body read failed: {}", error.without_url()))
}

/// Checks hourly and renames when due. A failed read keeps the applied names and retries at the
/// next check.
pub async fn run_names_refresher(
    http: reqwest::Client,
    url: String,
    names: SharedNames,
    every: Duration,
) {
    let mut ticker = tokio::time::interval(CHECK_EVERY);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        refresh_names_once(&http, &url, &names, every, SystemTime::now()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::Router;
    use std::path::PathBuf;

    const FOURTEEN_DAYS: Duration = Duration::from_secs(1_209_600);

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn applied(applied_at_unix: u64) -> AppliedNames {
        AppliedNames {
            inputs: StationNames::default(),
            applied_at_unix,
        }
    }

    fn body(fetched_at: &str) -> String {
        serde_json::json!({
            "directory": [
                {"code": "NYP", "name": "New York, NY", "city": "New York",
                 "state": "NY", "facilityName64": "Moynihan Train Hall"},
                {"code": "XX", "name": "Bad, ZZ", "city": "Bad"},
                {"code": "ABC", "name": "", "city": "Nowhere"},
                {"code": "DEF", "name": "Def, ST"},
            ],
            "directory_fetched_at": fetched_at,
            "seen": {
                "NYP": {"name": "New York Penn", "displayName": "New York",
                        "facility": "Penn Station", "audioName": "New York Penn Station"},
                "BOS": {"name": "Boston South"},
                "TOOLONG": {"name": "Skipped"},
            },
            "seen_updated_at": null,
        })
        .to_string()
    }

    fn path(label: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "amtrak-names-task-{label}-{}-{:?}",
                std::process::id(),
                SystemTime::now()
            ))
            .join("station-names-applied.json")
    }

    async fn serve(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}")
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap()
    }

    #[test]
    fn parse_stations_reads_worker_json() {
        let (names, fetched) = parse_stations(&body("2026-11-01T12:00:00+00:00")).unwrap();
        assert_eq!(names.directory.len(), 1);
        let nyp = &names.directory["NYP"];
        assert_eq!(nyp.name, "New York, NY");
        assert_eq!(nyp.city, "New York");
        assert_eq!(nyp.facility.as_deref(), Some("Moynihan Train Hall"));
        assert_eq!(names.seen.len(), 2);
        let seen = &names.seen["NYP"];
        assert_eq!(seen.name.as_deref(), Some("New York Penn"));
        assert_eq!(seen.display_name.as_deref(), Some("New York"));
        assert_eq!(seen.facility.as_deref(), Some("Penn Station"));
        assert_eq!(seen.audio_name.as_deref(), Some("New York Penn Station"));
        assert_eq!(names.seen["BOS"].display_name, None);
        assert_eq!(fetched, Some(at(1_793_534_400)));
        let (_, none) = parse_stations(r#"{"directory":[],"directory_fetched_at":null}"#).unwrap();
        assert_eq!(none, None);
        let (_, z) = parse_stations(r#"{"directory_fetched_at":"2026-11-01T12:00:00Z"}"#).unwrap();
        assert_eq!(z, Some(at(1_793_534_400)));
        let (_, naive) =
            parse_stations(r#"{"directory_fetched_at":"2026-11-01T12:00:00.5"}"#).unwrap();
        assert_eq!(naive, Some(at(1_793_534_400)));
        assert!(parse_stations("[]").is_err());
        assert!(parse_stations("not json").is_err());
    }

    #[test]
    fn first_renaming_waits_a_day() {
        let t = 1_000_000;
        assert!(!renaming_due(
            None,
            Some(at(t)),
            at(t + 86_399),
            FOURTEEN_DAYS
        ));
        assert!(renaming_due(
            None,
            Some(at(t)),
            at(t + 86_400),
            FOURTEEN_DAYS
        ));
        assert!(!renaming_due(None, None, at(t + 10_000_000), FOURTEEN_DAYS));
    }

    #[test]
    fn later_renamings_every_fourteen_days() {
        let a = 5_000_000;
        let applied = applied(a);
        // The directory's age no longer matters once something is applied.
        assert!(!renaming_due(
            Some(&applied),
            Some(at(0)),
            at(a + 1_209_599),
            FOURTEEN_DAYS
        ));
        assert!(renaming_due(
            Some(&applied),
            None,
            at(a + 1_209_600),
            FOURTEEN_DAYS
        ));
    }

    #[tokio::test]
    async fn due_read_applies_and_notifies() {
        let url = serve(Router::new().route(
            "/stations.json",
            get(|| async { body("2026-11-01T12:00:00+00:00") }),
        ))
        .await;
        let file = path("due");
        let names = SharedNames::load(file.clone());
        let now = at(1_793_534_400 + 86_400);
        // One second early: not due, nothing written.
        let early = at(1_793_534_400 + 86_399);
        let outcome = refresh_names_once(
            &client(),
            &format!("{url}/stations.json"),
            &names,
            FOURTEEN_DAYS,
            early,
        )
        .await;
        assert_eq!(outcome, NamesOutcome::Kept);
        assert!(names.applied().is_none() && !file.exists());
        let outcome = refresh_names_once(
            &client(),
            &format!("{url}/stations.json"),
            &names,
            FOURTEEN_DAYS,
            now,
        )
        .await;
        assert_eq!(outcome, NamesOutcome::Applied);
        let current = names.applied().unwrap();
        assert_eq!(current.applied_at_unix, 1_793_534_400 + 86_400);
        assert!(current.inputs.directory.contains_key("NYP"));
        assert!(file.exists());
        tokio::time::timeout(Duration::from_secs(1), names.changed().notified())
            .await
            .expect("the change was not signaled");
        assert!(SharedNames::load(file).applied().is_some());
        // Applied now, so the next check does not even ask the worker.
        let again = refresh_names_once(
            &client(),
            "http://127.0.0.1:1/unused",
            &names,
            FOURTEEN_DAYS,
            now + Duration::from_secs(3_600),
        )
        .await;
        assert_eq!(again, NamesOutcome::NotDue);
    }

    #[tokio::test]
    async fn failed_read_keeps_applied_names() {
        let url = serve(Router::new().route(
            "/stations.json",
            get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        ))
        .await;
        let names = SharedNames::load(path("failed"));
        names.set_applied(applied(100)).unwrap();
        // Take the permit `set_applied` stored, so only a later change could be seen.
        names.changed().notified().await;
        let before = names.applied().unwrap();
        let outcome = refresh_names_once(
            &client(),
            &format!("{url}/stations.json"),
            &names,
            FOURTEEN_DAYS,
            at(100 + 1_209_600),
        )
        .await;
        assert_eq!(outcome, NamesOutcome::Kept);
        assert_eq!(*names.applied().unwrap(), *before);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), names.changed().notified())
                .await
                .is_err(),
            "a failed read must not notify"
        );
    }

    #[tokio::test]
    async fn empty_names_are_never_applied() {
        let url = serve(Router::new().route(
            "/stations.json",
            get(|| async {
                r#"{"directory":[],"seen":{},"directory_fetched_at":"2026-01-01T00:00:00Z"}"#
            }),
        ))
        .await;
        let names = SharedNames::load(path("empty"));
        let outcome = refresh_names_once(
            &client(),
            &format!("{url}/stations.json"),
            &names,
            FOURTEEN_DAYS,
            at(2_000_000_000),
        )
        .await;
        assert_eq!(outcome, NamesOutcome::Kept);
        assert!(names.applied().is_none());
    }

    #[tokio::test]
    async fn read_errors_do_not_contain_the_url() {
        let error = read_stations(&client(), "http://127.0.0.1:1/secret-path/stations.json")
            .await
            .unwrap_err();
        assert!(error.starts_with("request failed"), "{error}");
        assert!(!error.contains("secret-path"), "{error}");
        assert!(!error.contains("127.0.0.1"), "{error}");
        // A bad status carries no URL either.
        let url = serve(Router::new().route(
            "/secret-path/stations.json",
            get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        ))
        .await;
        let error = read_stations(&client(), &format!("{url}/secret-path/stations.json"))
            .await
            .unwrap_err();
        assert!(!error.contains("secret-path"), "{error}");
    }

    #[tokio::test]
    async fn unreachable_worker_keeps_names() {
        let names = SharedNames::load(path("down"));
        names.set_applied(applied(1)).unwrap();
        let outcome = refresh_names_once(
            &client(),
            "http://127.0.0.1:1/stations.json",
            &names,
            FOURTEEN_DAYS,
            at(2_000_000_000),
        )
        .await;
        assert_eq!(outcome, NamesOutcome::Kept);
        assert_eq!(names.applied().unwrap().applied_at_unix, 1);
    }

    #[tokio::test]
    async fn empty_directory_never_replaces_applied_names() {
        let url = serve(Router::new().route(
            "/stations.json",
            get(|| async {
                r#"{"directory":[],"directory_fetched_at":null,"seen":{"BOS":{"name":"Boston South"}}}"#
            }),
        ))
        .await;
        let names = SharedNames::load(path("empty-dir"));
        names.set_applied(applied(100)).unwrap();
        names.changed().notified().await;
        let before = names.applied().unwrap();
        let outcome = refresh_names_once(
            &client(),
            &format!("{url}/stations.json"),
            &names,
            FOURTEEN_DAYS,
            at(100 + 1_209_600),
        )
        .await;
        assert_eq!(outcome, NamesOutcome::Kept);
        assert_eq!(*names.applied().unwrap(), *before);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), names.changed().notified())
                .await
                .is_err(),
            "an empty directory must not notify"
        );
    }

    #[test]
    fn huge_refresh_interval_is_never_due_and_does_not_panic() {
        let applied = applied(100);
        assert!(!renaming_due(
            Some(&applied),
            None,
            at(2_000_000_000),
            Duration::from_secs(u64::MAX)
        ));
        let huge = super::AppliedNames {
            inputs: StationNames::default(),
            applied_at_unix: u64::MAX,
        };
        assert!(!renaming_due(
            Some(&huge),
            None,
            at(2_000_000_000),
            FOURTEEN_DAYS
        ));
    }
}
