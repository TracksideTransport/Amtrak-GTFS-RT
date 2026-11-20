//! The train list the browser worker's track poller reads instead of polling Amtrak.
//!
//! The service is the only process that fetches `getTrainsData`. After each published
//! generation it writes this list, so the worker can pick trains near a stop without a second
//! map poll. Only matched trains appear: track stamps apply only to matched trip updates.
//!
//! The schema stays `amtrak-fleet-trains/v1`: every field added since is optional and readers
//! ignore fields they do not know. A stop where riders cannot board (static `pickup_type` 1)
//! carries `"boarding": false`; a missing field means boarding is allowed. With tracks on, the
//! top-level `track_stations` lists the stations with a configured or observed platform, the
//! stations where a track has ever been published; without it, every station may have one.
//!
//! In combined mode the list is one file, replaced atomically ([`WithFleetTrainsFile`]). A
//! failed write is logged and never fails the generation.

use crate::orchestrator::{GenerationCommitter, ValidatedGeneration};
use crate::static_augment::SharedPlatforms;
use crate::writer::{PublishError, PublishedGeneration};
use async_trait::async_trait;
use gtfs_realtime::trip_update::StopTimeUpdate;
use gtfs_realtime::FeedMessage;
use gtfs_structures::{Gtfs, PickupDropOffType};
use prost::Message;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Builds `trains.json` from matched trip updates.
///
/// `train_number` maps a trip id to its public train number (`trips.txt`
/// `trip_short_name`). Each stop lists its arrival and departure instants as unix
/// seconds, preferring the predicted time the trip update carries. A trip with no
/// train number, no start date, or no timed stop is left out. `boarding` says whether riders
/// can board that trip at that stop; a stop where they cannot gets `"boarding": false`.
/// `track_stations`, when given, is written as the top-level `track_stations` list.
pub fn train_list(
    trip_updates: &FeedMessage,
    train_number: impl Fn(&str) -> Option<String>,
    boarding: impl Fn(&str, &StopTimeUpdate) -> bool,
    track_stations: Option<&[String]>,
) -> Value {
    let mut trains = Vec::new();
    for entity in &trip_updates.entity {
        let Some(update) = entity.trip_update.as_ref() else {
            continue;
        };
        let Some(trip_id) = update.trip.trip_id.as_deref() else {
            continue;
        };
        let Some(number) = train_number(trip_id) else {
            continue;
        };
        let Some(service_date) = update.trip.start_date.as_deref().and_then(iso_date) else {
            continue;
        };
        let stops: Vec<Value> = update
            .stop_time_update
            .iter()
            .filter_map(|stop| {
                let code = stop.stop_id.as_deref()?.trim();
                let times: Vec<i64> = [stop.arrival.as_ref(), stop.departure.as_ref()]
                    .into_iter()
                    .flatten()
                    .filter_map(|event| event.time)
                    .collect();
                if code.is_empty() || times.is_empty() {
                    return None;
                }
                let mut entry = json!({ "code": code.to_ascii_uppercase(), "times_unix": times });
                if !boarding(trip_id, stop) {
                    entry["boarding"] = json!(false);
                }
                Some(entry)
            })
            .collect();
        if stops.is_empty() {
            continue;
        }
        trains.push(json!({
            "train_number": number,
            "service_date": service_date,
            "stops": stops,
        }));
    }
    let mut list = json!({ "schema": "amtrak-fleet-trains/v1", "trains": trains });
    if let Some(stations) = track_stations {
        list["track_stations"] = json!(stations);
    }
    list
}

/// Whether riders can board trip `trip_id` at the stop `stop` updates.
///
/// The stop is found in the static `stop_times.txt` by `stop_sequence`, or by `stop_id` when the
/// update has no sequence or its sequence is not in the trip. Only `pickup_type` 1 (no pickup) refuses boarding; a trip or stop the
/// static feed does not have is treated as boardable.
pub fn boarding_allowed(gtfs: &Gtfs, trip_id: &str, stop: &StopTimeUpdate) -> bool {
    let Some(trip) = gtfs.trips.get(trip_id) else {
        return true;
    };
    let static_stop = stop
        .stop_sequence
        .and_then(|sequence| {
            trip.stop_times
                .iter()
                .find(|time| time.stop_sequence == sequence)
        })
        .or_else(|| {
            let id = stop.stop_id.as_deref()?.trim();
            trip.stop_times
                .iter()
                .find(|time| time.stop.id.eq_ignore_ascii_case(id))
        });
    !static_stop.is_some_and(|time| time.pickup_type == PickupDropOffType::NotAvailable)
}

/// `trains.json` bytes for one validated generation.
pub fn generation_train_list(
    candidate: &ValidatedGeneration,
    track_stations: Option<&[String]>,
) -> Vec<u8> {
    let trip_updates = FeedMessage::decode(candidate.trip_updates.as_ref()).unwrap_or_default();
    let gtfs = &candidate.static_snapshot.parsed;
    train_list(
        &trip_updates,
        |trip_id| gtfs.trips.get(trip_id)?.trip_short_name.clone(),
        |trip_id, stop| boarding_allowed(gtfs, trip_id, stop),
        track_stations,
    )
    .to_string()
    .into_bytes()
}

/// Publishes through `inner`, then replaces `path` (when set) with that generation's train list.
///
/// The file is written only after the generation is published, through a temporary file and a
/// rename, so the worker never reads a partial list. A write failure is logged and the
/// generation still counts as published.
pub struct WithFleetTrainsFile<C> {
    inner: C,
    path: Option<PathBuf>,
    platforms: Option<SharedPlatforms>,
}

impl<C> WithFleetTrainsFile<C> {
    /// Wraps `inner`; the list is written to `path`. `None` publishes through `inner` only.
    pub fn new(inner: C, path: Option<PathBuf>) -> Self {
        Self {
            inner,
            path,
            platforms: None,
        }
    }

    /// Lists the stations of `platforms` (configured and observed) as `track_stations`, read
    /// again for each generation so a newly observed track counts from the next list on.
    pub fn with_track_stations(mut self, platforms: Option<SharedPlatforms>) -> Self {
        self.platforms = platforms;
        self
    }
}

#[async_trait]
impl<C: GenerationCommitter> GenerationCommitter for WithFleetTrainsFile<C> {
    async fn publish(
        &self,
        candidate: ValidatedGeneration,
    ) -> Result<Arc<PublishedGeneration>, PublishError> {
        let Some(path) = self.path.clone() else {
            return self.inner.publish(candidate).await;
        };
        let stations: Option<Vec<String>> = self.platforms.as_ref().map(|platforms| {
            platforms
                .snapshot()
                .station_ids()
                .map(str::to_owned)
                .collect()
        });
        let list = generation_train_list(&candidate, stations.as_deref());
        let published = self.inner.publish(candidate).await?;
        let written = tokio::task::spawn_blocking(move || replace_file(&path, &list)).await;
        if !matches!(written, Ok(Ok(()))) {
            tracing::warn!(
                "fleet train list was not written; Amtrak track polls keep the last one"
            );
        }
        Ok(published)
    }
}

fn replace_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("fleet-trains.json");
    let temporary = parent.join(format!(".{name}.tmp"));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}

/// `20261107` becomes `2026-11-07`; anything else is rejected.
fn iso_date(raw: &str) -> Option<String> {
    let raw = raw.trim();
    (raw.len() == 8 && raw.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| format!("{}-{}-{}", &raw[..4], &raw[4..6], &raw[6..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtfs_realtime::trip_update::{StopTimeEvent, StopTimeUpdate};
    use gtfs_realtime::{FeedEntity, TripDescriptor, TripUpdate};

    fn event(time: i64) -> Option<StopTimeEvent> {
        Some(StopTimeEvent {
            time: Some(time),
            ..Default::default()
        })
    }

    fn update(trip_id: &str, start_date: &str, stops: Vec<StopTimeUpdate>) -> FeedEntity {
        FeedEntity {
            id: trip_id.into(),
            trip_update: Some(TripUpdate {
                trip: TripDescriptor {
                    trip_id: Some(trip_id.into()),
                    start_date: Some(start_date.into()),
                    ..Default::default()
                },
                stop_time_update: stops,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn stop(code: &str, arrival: Option<i64>, departure: Option<i64>) -> StopTimeUpdate {
        StopTimeUpdate {
            stop_id: Some(code.into()),
            arrival: arrival.and_then(event),
            departure: departure.and_then(event),
            ..Default::default()
        }
    }

    fn numbers(trip_id: &str) -> Option<String> {
        match trip_id {
            "trip-163" => Some("163".into()),
            "trip-66" => Some("66".into()),
            _ => None,
        }
    }

    #[test]
    fn lists_matched_trains_with_station_times() {
        let feed = FeedMessage {
            entity: vec![update(
                "trip-163",
                "20261107",
                vec![
                    stop("nhv", Some(1_794_081_000), Some(1_794_081_300)),
                    stop("NYP", Some(1_794_088_000), None),
                ],
            )],
            ..Default::default()
        };
        assert_eq!(
            train_list(&feed, numbers, |_, _| true, None),
            json!({
                "schema": "amtrak-fleet-trains/v1",
                "trains": [{
                    "train_number": "163",
                    "service_date": "2026-11-07",
                    "stops": [
                        { "code": "NHV", "times_unix": [1_794_081_000, 1_794_081_300] },
                        { "code": "NYP", "times_unix": [1_794_088_000] },
                    ],
                }],
            })
        );
    }

    #[test]
    fn marks_stops_where_riders_cannot_board() {
        let feed = FeedMessage {
            entity: vec![update(
                "trip-163",
                "20261119",
                vec![
                    stop("PVD", Some(1_795_118_000), Some(1_795_118_100)),
                    stop("RTE", Some(1_795_120_000), Some(1_795_120_060)),
                ],
            )],
            ..Default::default()
        };
        let list = train_list(
            &feed,
            numbers,
            |_, stop| stop.stop_id.as_deref() != Some("RTE"),
            None,
        );
        // A stop riders can board carries no field, so older readers and a missing field agree.
        assert_eq!(
            list["trains"][0]["stops"],
            json!([
                { "code": "PVD", "times_unix": [1_795_118_000, 1_795_118_100] },
                { "code": "RTE", "times_unix": [1_795_120_000, 1_795_120_060], "boarding": false },
            ])
        );
        assert_eq!(list["schema"], "amtrak-fleet-trains/v1");
        assert!(list.get("track_stations").is_none());
    }

    #[test]
    fn lists_the_stations_that_have_tracks_when_known() {
        let feed = FeedMessage::default();
        let stations = ["NHV".to_string(), "NYP".to_string(), "PVD".to_string()];
        let list = train_list(&feed, numbers, |_, _| true, Some(&stations));
        assert_eq!(list["track_stations"], json!(["NHV", "NYP", "PVD"]));
    }

    #[test]
    fn boarding_follows_the_static_pickup_type() {
        use gtfs_structures::{Gtfs, PickupDropOffType, Stop, StopTime, Trip};
        let stop_time = |id: &str, sequence: u32, pickup_type| StopTime {
            stop: Arc::new(Stop {
                id: id.into(),
                ..Default::default()
            }),
            stop_sequence: sequence,
            pickup_type,
            ..Default::default()
        };
        let mut gtfs = Gtfs::default();
        gtfs.trips.insert(
            "trip-172".into(),
            Trip {
                id: "trip-172".into(),
                trip_short_name: Some("172".into()),
                stop_times: vec![
                    stop_time("PVD", 22, PickupDropOffType::Regular),
                    stop_time("RTE", 23, PickupDropOffType::NotAvailable),
                    stop_time("BOS", 25, PickupDropOffType::ArrangeByPhone),
                ],
                ..Default::default()
            },
        );
        let by_sequence = StopTimeUpdate {
            stop_sequence: Some(23),
            ..Default::default()
        };
        assert!(!boarding_allowed(&gtfs, "trip-172", &by_sequence));
        assert!(!boarding_allowed(
            &gtfs,
            "trip-172",
            &stop("rte", Some(1), None)
        ));
        assert!(boarding_allowed(
            &gtfs,
            "trip-172",
            &stop("PVD", Some(1), None)
        ));
        // Only pickup_type 1 means no boarding; an unknown trip or stop is boardable.
        assert!(boarding_allowed(
            &gtfs,
            "trip-172",
            &stop("BOS", Some(1), None)
        ));
        assert!(boarding_allowed(
            &gtfs,
            "trip-172",
            &stop("NYP", Some(1), None)
        ));
        assert!(boarding_allowed(
            &gtfs,
            "other-trip",
            &stop("RTE", Some(1), None)
        ));
    }

    #[test]
    fn skips_trips_without_a_number_date_or_timed_stop() {
        let feed = FeedMessage {
            entity: vec![
                update("unknown-trip", "20261107", vec![stop("NYP", Some(1), None)]),
                update("trip-66", "11/07/2026", vec![stop("BOS", Some(1), None)]),
                update("trip-163", "20261107", vec![stop("NYP", None, None)]),
            ],
            ..Default::default()
        };
        assert_eq!(
            train_list(&feed, numbers, |_, _| true, None)["trains"],
            json!([])
        );
    }
}
