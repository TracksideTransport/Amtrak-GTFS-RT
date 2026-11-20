//! Fallback realtime source: TransitDocs' ASM train map (`asm-backend.transitdocs.com/map`).
//!
//! It runs only after the primary Amtrak source returned an error or an empty batch (the
//! orchestrator's source chain stops at the first non-empty batch), so on a healthy cycle it makes
//! no request. The primary crate already reads this same endpoint for alerts; the fallback adds at
//! most one request per cycle, and only while the primary is failing.
//!
//! The map lists each train with its number, origin (service) date, position, and per-stop
//! scheduled and actual or estimated times. Each Amtrak train is matched to a static trip by
//! `trip_short_name` (the train number) and the service calendar on its origin date, exactly as
//! Amtrak's own GTFS names its trips. A train with no matching active trip is omitted, so every
//! trip and stop id in the batch resolves against the static feed. Delays are the map's `variance`
//! values, which are seconds. Positions are in mph, converted to metres per second. A train is
//! identified as the Amtrak source identifies it ([`crate::sources::identity`]): entity id and
//! `vehicle.id` `{YYYYMMDD}-{number}` from its origin date, `vehicle.label` the number, on the trip
//! update and the vehicle position. The map's `name` is the route and is not published.
//!
//! This is a clean-room client written from the endpoint's public JSON, not a copy of any other
//! project's code. Credit and terms-of-use notes are in the README ("Fallback position source").

use crate::sources::{identity, RtBatch, RtSource, SourceError};
use async_trait::async_trait;
use chrono::NaiveDate;
use gtfs_realtime::trip_update::{
    stop_time_update::ScheduleRelationship, StopTimeEvent, StopTimeUpdate,
};
use gtfs_realtime::{
    FeedEntity, FeedMessage, Position, TripDescriptor, TripUpdate, VehiclePosition,
};
use gtfs_structures::{Exception, Gtfs, Trip};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default `AMTRAK_FALLBACK_ASM_URL`.
pub const DEFAULT_ASM_URL: &str = "https://asm-backend.transitdocs.com/map";

/// Name published in the manifest's `source` field when this source supplies a generation.
pub const SOURCE_NAME: &str = "transitdocs-asm";

/// A train whose last update is older than this is not published.
const MAX_AGE_SECS: u64 = 15 * 60;

const MPH_TO_METRES_PER_SECOND: f32 = 0.447_04;

/// The ASM map as a realtime source.
pub struct AsmSource {
    client: reqwest::Client,
    url: String,
}

impl AsmSource {
    /// A source that reads `url`.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            url: url.into(),
        }
    }
}

#[async_trait]
impl RtSource for AsmSource {
    fn name(&self) -> &'static str {
        SOURCE_NAME
    }

    async fn fetch(&self, gtfs: &Gtfs) -> Result<RtBatch, SourceError> {
        let text = self
            .client
            .get(&self.url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|_| "fallback map request failed")?
            .error_for_status()
            .map_err(|_| "fallback map status failed")?
            .text()
            .await
            .map_err(|_| "fallback map body failed")?;
        let body: Value =
            serde_json::from_str(&text).map_err(|_| "fallback map body was not JSON")?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        Ok(batch_from_map(&body, gtfs, now))
    }
}

/// Builds a batch from the ASM map `body`, keeping only trains that resolve against `gtfs`.
///
/// `now_unix` bounds staleness. A body that is not an array, or has no usable train, gives an empty
/// batch, which the orchestrator treats as "this source has nothing" and tries the next one.
pub fn batch_from_map(body: &Value, gtfs: &Gtfs, now_unix: u64) -> RtBatch {
    let mut trip_updates = FeedMessage::default();
    let mut vehicle_positions = FeedMessage::default();
    let Some(trains) = body.as_array() else {
        return RtBatch {
            trip_updates,
            vehicle_positions,
            alerts: FeedMessage::default(),
        };
    };
    for train in trains {
        if let Some(entity) = entity_for_train(train, gtfs, now_unix) {
            trip_updates.entity.push(entity.clone());
            vehicle_positions.entity.push(entity);
        }
    }
    RtBatch {
        trip_updates,
        vehicle_positions,
        alerts: FeedMessage::default(),
    }
}

fn entity_for_train(train: &Value, gtfs: &Gtfs, now_unix: u64) -> Option<FeedEntity> {
    if train.get("railroad")?.as_str()? != "AMTRAK" {
        return None;
    }
    let number = train.get("number")?.as_u64()?.to_string();
    let date = NaiveDate::parse_from_str(train.get("origin_date")?.as_str()?, "%Y-%m-%d").ok()?;
    let updated = train.get("last_updated")?.as_u64()?;
    if now_unix.saturating_sub(updated) > MAX_AGE_SECS {
        return None;
    }
    let trip = trip_for(gtfs, &number, date)?;
    let start_date = date.format("%Y%m%d").to_string();
    // The Amtrak source's identity (#122): the map's `name` is the route, not the train.
    let identified = identity::descriptor(&start_date, &number);
    let descriptor = TripDescriptor {
        trip_id: Some(trip.id.clone()),
        route_id: Some(trip.route_id.clone()),
        start_date: Some(start_date.clone()),
        ..Default::default()
    };
    let vehicle = train.get("location").and_then(|location| {
        let latitude = location.get("latitude")?.as_f64()? as f32;
        let longitude = location.get("longitude")?.as_f64()? as f32;
        Some(VehiclePosition {
            trip: Some(descriptor.clone()),
            vehicle: Some(identified.clone()),
            position: Some(Position {
                latitude,
                longitude,
                bearing: location
                    .get("heading")
                    .and_then(Value::as_f64)
                    .map(|degrees| degrees as f32),
                speed: location
                    .get("speed")
                    .and_then(Value::as_f64)
                    .map(|mph| mph as f32 * MPH_TO_METRES_PER_SECOND),
                ..Default::default()
            }),
            timestamp: Some(updated),
            ..Default::default()
        })
    });
    let stop_updates = train
        .get("stops")
        .and_then(Value::as_array)
        .map(|stops| {
            stops
                .iter()
                .filter_map(|stop| stop_update(stop, trip))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if vehicle.is_none() && stop_updates.is_empty() {
        return None;
    }
    Some(FeedEntity {
        id: identity::train_day_id(&start_date, &number),
        trip_update: Some(TripUpdate {
            trip: descriptor,
            vehicle: Some(identified),
            stop_time_update: stop_updates,
            timestamp: Some(updated),
            ..Default::default()
        }),
        vehicle,
        ..Default::default()
    })
}

/// One stop's update, or `None` when the stop is not on the trip exactly once.
fn stop_update(stop: &Value, trip: &Trip) -> Option<StopTimeUpdate> {
    let code = stop.get("code")?.as_str()?;
    let mut on_trip = trip.stop_times.iter().filter(|time| time.stop.id == code);
    let scheduled = on_trip.next()?;
    if on_trip.next().is_some() {
        // A stop the trip calls at twice cannot be told apart by code alone.
        return None;
    }
    let mut update = StopTimeUpdate {
        stop_sequence: Some(scheduled.stop_sequence),
        stop_id: Some(code.to_string()),
        ..Default::default()
    };
    if stop.get("canceled").and_then(Value::as_bool) == Some(true) {
        update.schedule_relationship = Some(ScheduleRelationship::Skipped as i32);
        return Some(update);
    }
    update.arrival = event(stop.get("arrive"));
    update.departure = event(stop.get("depart"));
    (update.arrival.is_some() || update.departure.is_some()).then_some(update)
}

/// A delay in seconds, unless the map flagged the reading as a mismatch.
fn event(reading: Option<&Value>) -> Option<StopTimeEvent> {
    let reading = reading?;
    if reading.get("type").and_then(Value::as_str) == Some("MISMATCH_ARRIVAL") {
        return None;
    }
    let delay = i32::try_from(reading.get("variance")?.as_i64()?).ok()?;
    Some(StopTimeEvent {
        delay: Some(delay),
        ..Default::default()
    })
}

/// The static trip for `number` whose service runs on `date`. When several qualify, the lowest
/// trip id wins, so the choice is stable from cycle to cycle.
fn trip_for<'a>(gtfs: &'a Gtfs, number: &str, date: NaiveDate) -> Option<&'a Trip> {
    gtfs.trips
        .values()
        .filter(|trip| trip.trip_short_name.as_deref() == Some(number))
        .filter(|trip| service_runs(gtfs, &trip.service_id, date))
        .min_by(|left, right| left.id.cmp(&right.id))
}

/// GTFS service calendar: a `calendar_dates` exception on `date` decides; otherwise the weekly
/// `calendar` row applies within its date range.
fn service_runs(gtfs: &Gtfs, service_id: &str, date: NaiveDate) -> bool {
    if let Some(exception) = gtfs
        .calendar_dates
        .get(service_id)
        .and_then(|dates| dates.iter().find(|entry| entry.date == date))
    {
        return exception.exception_type == Exception::Added;
    }
    gtfs.calendar.get(service_id).is_some_and(|calendar| {
        date >= calendar.start_date && date <= calendar.end_date && calendar.valid_weekday(date)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    /// Train 785 runs Mon-Fri 2026-11-01 to 2026-12-31 (`wk`); train 790 only runs on an added
    /// date (`one`); train 91 exists but its service is deleted on 2026-11-17 (`gone`).
    fn gtfs() -> Gtfs {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        for (name, contents) in [
            ("agency.txt", "agency_id,agency_name,agency_url,agency_timezone\na,Amtrak,https://amtrak.com,America/Los_Angeles\n"),
            ("stops.txt", "stop_id,stop_name,stop_lat,stop_lon\nSAN,San Diego,32.7,-117.1\nGTA,Glendale,34.1,-118.2\nSBA,Santa Barbara,34.4,-119.7\n"),
            ("routes.txt", "route_id,agency_id,route_short_name,route_long_name,route_type\nr,a,PS,Pacific Surfliner,2\n"),
            ("trips.txt", "route_id,service_id,trip_id,trip_short_name\nr,wk,t785,785\nr,one,t790,790\nr,gone,t91,91\n"),
            ("stop_times.txt", "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nt785,10:00:00,10:00:00,SAN,1\nt785,11:00:00,11:05:00,GTA,2\nt785,12:00:00,12:00:00,SBA,3\nt790,10:00:00,10:00:00,GTA,1\nt91,09:00:00,09:00:00,SAN,1\n"),
            ("calendar.txt", "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nwk,1,1,1,1,1,0,0,20261101,20261231\ngone,1,1,1,1,1,1,1,20261101,20261231\n"),
            ("calendar_dates.txt", "service_id,date,exception_type\none,20261117,1\ngone,20261117,2\n"),
        ] {
            archive.start_file(name, options).unwrap();
            archive.write_all(contents.as_bytes()).unwrap();
        }
        Gtfs::from_reader(Cursor::new(archive.finish().unwrap().into_inner())).unwrap()
    }

    fn train(number: u64, extra: Value) -> Value {
        let mut base = json!({
            "railroad": "AMTRAK",
            "number": number,
            "name": "Pacific Surfliner",
            "origin_date": "2026-11-17",
            "last_updated": 1_000_000,
            "location": {"latitude": 34.2, "longitude": -118.4, "heading": 270, "speed": 60.0},
            "stops": [
                {"code": "SAN", "canceled": false,
                 "depart": {"variance": 0, "type": "ACTUAL"}},
                {"code": "GTA", "canceled": false,
                 "arrive": {"variance": 120, "type": "ESTIMATED"},
                 "depart": {"variance": -60, "type": "ESTIMATED"}},
                {"code": "SBA", "canceled": true},
                {"code": "ZZZ", "canceled": false, "depart": {"variance": 5, "type": "ACTUAL"}}
            ]
        });
        for (key, value) in extra.as_object().unwrap() {
            base[key] = value.clone();
        }
        base
    }

    const NOW: u64 = 1_000_100;

    #[test]
    fn a_train_resolves_to_its_trip_and_stops_with_delays_in_seconds() {
        let batch = batch_from_map(&json!([train(785, json!({}))]), &gtfs(), NOW);
        assert_eq!(batch.trip_updates.entity.len(), 1);
        let entity = &batch.trip_updates.entity[0];
        // #122: the Amtrak source's id scheme, `{start_date}-{number}`.
        assert_eq!(entity.id, "20261117-785");
        let update = entity.trip_update.as_ref().unwrap();
        assert_eq!(update.trip.trip_id.as_deref(), Some("t785"));
        assert_eq!(update.trip.route_id.as_deref(), Some("r"));
        assert_eq!(update.trip.start_date.as_deref(), Some("20261117"));
        // ZZZ is not on the trip and is dropped; the other three keep their sequence and stop id.
        let stops: Vec<_> = update
            .stop_time_update
            .iter()
            .map(|stop| (stop.stop_sequence.unwrap(), stop.stop_id.clone().unwrap()))
            .collect();
        assert_eq!(
            stops,
            vec![(1, "SAN".into()), (2, "GTA".into()), (3, "SBA".into())]
        );
        let gta = &update.stop_time_update[1];
        assert_eq!(gta.arrival.as_ref().unwrap().delay, Some(120));
        assert_eq!(gta.departure.as_ref().unwrap().delay, Some(-60));
        assert_eq!(
            update.stop_time_update[2].schedule_relationship,
            Some(ScheduleRelationship::Skipped as i32)
        );
        let vehicle = entity.vehicle.as_ref().unwrap();
        let position = vehicle.position.as_ref().unwrap();
        assert!((position.latitude - 34.2).abs() < 1e-4);
        assert_eq!(position.bearing, Some(270.0));
        assert!(
            (position.speed.unwrap() - 26.8224).abs() < 1e-3,
            "mph to m/s"
        );
        assert_eq!(vehicle.timestamp, Some(1_000_000));
        // Both products carry the same entity, as the primary source's unified feed does.
        assert_eq!(batch.vehicle_positions.entity.len(), 1);
    }

    /// #122: the trip update and the vehicle position carry the Amtrak source's identity, the
    /// per-train-day id and the train number, never the route name the map calls `name`.
    #[test]
    fn both_products_carry_the_train_day_id_and_the_number_not_the_route_name() {
        let batch = batch_from_map(&json!([train(785, json!({}))]), &gtfs(), NOW);
        for entity in [
            &batch.trip_updates.entity[0],
            &batch.vehicle_positions.entity[0],
        ] {
            for vehicle in [
                entity.trip_update.as_ref().unwrap().vehicle.as_ref(),
                entity.vehicle.as_ref().unwrap().vehicle.as_ref(),
            ] {
                let vehicle = vehicle.expect("a vehicle descriptor");
                assert_eq!(vehicle.id.as_deref(), Some("20261117-785"));
                assert_eq!(vehicle.label.as_deref(), Some("785"));
            }
        }
    }

    /// #122: one train has the same entity id and descriptor whichever source built the
    /// generation. The primary's entity is shaped as the crate encodes it.
    #[test]
    fn the_fallback_and_the_primary_identify_a_train_the_same_way() {
        use crate::sources::identity::identify;
        let fallback = batch_from_map(&json!([train(785, json!({}))]), &gtfs(), NOW);
        let mut primary = FeedMessage {
            entity: vec![FeedEntity {
                id: "20261117-785".into(),
                trip_update: Some(TripUpdate::default()),
                vehicle: Some(VehiclePosition::default()),
                ..Default::default()
            }],
            ..Default::default()
        };
        identify(&mut primary);
        let (left, right) = (&fallback.vehicle_positions.entity[0], &primary.entity[0]);
        assert_eq!(left.id, right.id);
        assert_eq!(
            left.vehicle.as_ref().unwrap().vehicle,
            right.vehicle.as_ref().unwrap().vehicle
        );
        assert_eq!(
            left.trip_update.as_ref().unwrap().vehicle,
            right.trip_update.as_ref().unwrap().vehicle
        );
    }

    #[test]
    fn only_trains_with_an_active_matching_trip_are_published() {
        let trains = json!([
            train(785, json!({})),
            // Runs only on an added date, and that date is the origin date.
            train(790, json!({})),
            // The service is deleted on 2026-11-17.
            train(91, json!({})),
            // No such train number in the static feed.
            train(9999, json!({})),
            // 785 on a Saturday: the weekday calendar does not run.
            train(785, json!({"origin_date": "2026-11-21"})),
            train(785, json!({"railroad": "VIA_RAIL"})),
        ]);
        let batch = batch_from_map(&trains, &gtfs(), NOW);
        let ids: Vec<_> = batch
            .trip_updates
            .entity
            .iter()
            .map(|e| e.id.clone())
            .collect();
        assert_eq!(ids, vec!["20261117-785", "20261117-790"]);
    }

    #[test]
    fn stale_trains_and_unusable_bodies_give_an_empty_batch() {
        let stale = batch_from_map(
            &json!([train(785, json!({"last_updated": 1_000_000 - 1}))]),
            &gtfs(),
            1_000_000 + MAX_AGE_SECS + 1,
        );
        assert!(stale.trip_updates.entity.is_empty());
        for body in [
            json!({}),
            json!("nope"),
            json!([]),
            json!([{"railroad": "AMTRAK"}]),
        ] {
            let batch = batch_from_map(&body, &gtfs(), NOW);
            assert!(batch.trip_updates.entity.is_empty(), "{body}");
            assert!(batch.vehicle_positions.entity.is_empty());
        }
    }

    #[test]
    fn a_mismatched_reading_gives_no_delay_and_a_train_without_position_still_publishes() {
        let mut one = train(
            785,
            json!({"location": null, "stops": [
                {"code": "SAN", "arrive": {"variance": 300, "type": "MISMATCH_ARRIVAL"}},
                {"code": "GTA", "arrive": {"variance": 60, "type": "ACTUAL"}}
            ]}),
        );
        one["location"] = Value::Null;
        let batch = batch_from_map(&json!([one]), &gtfs(), NOW);
        let entity = &batch.trip_updates.entity[0];
        assert!(entity.vehicle.is_none());
        // The trip update still names its train.
        let named = entity
            .trip_update
            .as_ref()
            .unwrap()
            .vehicle
            .as_ref()
            .unwrap();
        assert_eq!(named.id.as_deref(), Some("20261117-785"));
        assert_eq!(named.label.as_deref(), Some("785"));
        let stops = &entity.trip_update.as_ref().unwrap().stop_time_update;
        assert_eq!(
            stops.len(),
            1,
            "the mismatch reading leaves SAN with no event"
        );
        assert_eq!(stops[0].stop_id.as_deref(), Some("GTA"));
    }

    /// Serves `body` as JSON from a local listener and returns its URL.
    async fn served_map(body: String) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        (format!("http://{address}/map"), server)
    }

    /// #62: when the Amtrak source fails, the chain falls through to this one, and everything it
    /// publishes resolves against the static feed, so the builder keeps all of it.
    #[tokio::test]
    async fn the_chain_falls_through_to_the_fallback_and_every_id_resolves() {
        use crate::orchestrator::{select_batch, GenerationBuilder, StaticSnapshot};
        use crate::sources::mock::{Behavior, MockSource};
        use std::sync::Arc;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let body = json!([
            train(785, json!({"last_updated": now})),
            train(790, json!({"last_updated": now}))
        ]);
        let (url, server) = served_map(body.to_string()).await;
        let gtfs = gtfs();
        let chain: Vec<Box<dyn RtSource>> = vec![
            Box::new(MockSource {
                name: "amtrak",
                behavior: Behavior::Fail,
            }),
            Box::new(AsmSource::new(url)),
        ];
        let selected = select_batch(&chain, &gtfs).await.unwrap();
        assert_eq!(selected.source_name, SOURCE_NAME);

        let snapshot = Arc::new(StaticSnapshot::identical(
            "STATIC-V1",
            Arc::new(gtfs),
            Arc::from(&b"PK fixture"[..]),
        ));
        let candidate =
            GenerationBuilder::build(snapshot, selected, UNIX_EPOCH + Duration::from_secs(now))
                .unwrap();
        let rate = candidate.match_rate;
        assert_eq!(rate.trip_updates_emitted, 2);
        assert_eq!(
            rate.trip_updates_matched, 2,
            "every trip and stop id resolves"
        );
        assert_eq!(rate.vehicle_positions_matched, 2);
        server.abort();
    }

    /// An Amtrak source that returns data means the fallback is never asked.
    #[tokio::test]
    async fn a_healthy_primary_never_reaches_the_fallback() {
        use crate::orchestrator::select_batch;
        use crate::sources::mock::{batch_with, Behavior, MockSource};
        let chain: Vec<Box<dyn RtSource>> = vec![
            Box::new(MockSource {
                name: "amtrak",
                behavior: Behavior::Ok(batch_with(1)),
            }),
            // Nothing listens here; a request would fail the chain's second link.
            Box::new(AsmSource::new("http://127.0.0.1:1/map")),
        ];
        let selected = select_batch(&chain, &gtfs()).await.unwrap();
        assert_eq!(selected.source_name, "amtrak");
    }

    /// Live: how much of the real ASM map resolves against Amtrak's real static feed. Run with
    /// `cargo test sources::asm -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "requires the live ASM map and Amtrak's static GTFS"]
    async fn live_map_resolves_against_the_static_feed() {
        let gtfs = Gtfs::from_url_async("https://content.amtrak.com/content/gtfs/GTFS.zip")
            .await
            .unwrap();
        let body: Value = reqwest::get(DEFAULT_ASM_URL)
            .await
            .unwrap()
            .text()
            .await
            .map(|text| serde_json::from_str(&text).unwrap())
            .unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let amtrak = body
            .as_array()
            .unwrap()
            .iter()
            .filter(|train| train["railroad"] == "AMTRAK")
            .count();
        let batch = batch_from_map(&body, &gtfs, now);
        println!(
            "ASM: {amtrak} Amtrak trains, {} resolved to a static trip",
            batch.trip_updates.entity.len()
        );
        assert!(amtrak > 0);
        assert!(
            batch.trip_updates.entity.len() * 100 >= amtrak * 50,
            "fewer than half of the live trains resolved"
        );
    }

    #[test]
    fn the_source_is_named_for_the_manifest() {
        assert_eq!(
            AsmSource::new("http://127.0.0.1:1/").name(),
            "transitdocs-asm"
        );
    }
}
