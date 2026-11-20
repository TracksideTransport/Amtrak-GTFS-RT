//! Writes one deterministic, network-free feed set for the MobilityData validators.
//!
//! `cargo run --locked --example validation_fixture -- OUT_DIR` builds a tiny but valid static GTFS
//! (three stations, one trip), augments it with platform stops exactly as the service does when
//! tracks are on (a parent station, a gated platform, and a plain platform at New York), and runs a
//! mock realtime batch through the real generation builder and validator. It then writes the files
//! `scripts/validate-feeds.sh --fixture` validates:
//!
//! - `official-static.zip`: the fixture before augmentation
//! - `static.zip`: the augmented feed the service would publish
//! - `trip-updates.pb`, `vehicle-positions.pb`, `alerts.pb`
//!
//! The train is shaped as the primary crate encodes it (one unified entity with the per-train-day
//! id `{YYYYMMDD}-123` and no vehicle descriptor, copied into each product) and goes through the
//! primary source's own conversion, so `vehicle.id` and `vehicle.label` come from the service's
//! code, not from the fixture. The example checks that both products carry the same `vehicle.id`,
//! and the script fails if the realtime validator reports W002 (`vehicle_id` not populated).
//!
//! The batch stamps `assigned_stop_id` the way the track source does (`stop_sequence` set, `stop_id`
//! cleared), so platform and gate stops are exercised. The fixture has none of the live-data
//! mismatches behind the entries in `validation/baseline.json`, so the validators must report zero
//! ERROR notices.
//!
//! The pinned GTFS-Realtime validator does not check that an `assigned_stop_id` names a real stop,
//! and neither does the generation builder, so this example does: it fails when a stamped platform
//! is not a stop in the augmented feed under the scheduled station's parent.

use amtrak_gtfs_rt_service::orchestrator::{
    CandidateValidator, GenerationBuilder, SelectedBatch, StaticSnapshot,
};
use amtrak_gtfs_rt_service::sources::amtrak::batch_from_results;
use amtrak_gtfs_rt_service::static_augment::{augment_static, PlatformTable};
use gtfs_realtime::trip_update::stop_time_update::StopTimeProperties;
use gtfs_realtime::trip_update::{StopTimeEvent, StopTimeUpdate};
use gtfs_realtime::{
    alert, translated_string, Alert, EntitySelector, FeedEntity, FeedMessage, Position, TimeRange,
    TranslatedString, TripDescriptor, TripUpdate, VehiclePosition,
};
use gtfs_structures::Gtfs;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use zip::write::SimpleFileOptions;

/// The platform stop the fixture's train is stamped with, and the table that creates it.
const PLATFORM: &str = "NYP:track:7:gate:A";
const PLATFORM_TABLE: &str = "NYP=7@A,8";

const STATIC_FILES: &[(&str, &str)] = &[
    (
        "agency.txt",
        "agency_id,agency_name,agency_url,agency_timezone,agency_lang\n\
         AMTRAK,Amtrak,https://www.amtrak.com,America/New_York,en\n",
    ),
    (
        "stops.txt",
        "stop_id,stop_name,stop_lat,stop_lon\n\
         NYP,New York Penn Station,40.750568,-73.993519\n\
         PHL,Philadelphia 30th Street,39.955823,-75.182193\n\
         WAS,Washington Union Station,38.897303,-77.006432\n",
    ),
    (
        "routes.txt",
        "route_id,agency_id,route_short_name,route_long_name,route_type\n\
         NEC,AMTRAK,NER,Northeast Regional,2\n",
    ),
    (
        "trips.txt",
        "route_id,service_id,trip_id,trip_headsign,trip_short_name,direction_id\n\
         NEC,daily,fixture-123,Washington,123,0\n",
    ),
    (
        "stop_times.txt",
        "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
         fixture-123,08:00:00,08:00:00,NYP,1\n\
         fixture-123,09:30:00,09:35:00,PHL,2\n\
         fixture-123,11:30:00,11:30:00,WAS,3\n",
    ),
    (
        "calendar.txt",
        "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
         daily,1,1,1,1,1,1,1,20200101,20991231\n",
    ),
    (
        "feed_info.txt",
        "feed_publisher_name,feed_publisher_url,feed_lang,feed_start_date,feed_end_date,feed_version\n\
         Amtrak GTFS-RT validation fixture,https://github.com/sohampatwardhan/Amtrak-GTFS-RT,en,20200101,20991231,FIXTURE\n",
    ),
];

fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, contents) in files {
        archive
            .start_file(*name, SimpleFileOptions::default())
            .expect("start zip entry");
        archive
            .write_all(contents.as_bytes())
            .expect("write zip entry");
    }
    archive.finish().expect("finish zip").into_inner()
}

fn text(value: &str) -> TranslatedString {
    TranslatedString {
        translation: vec![translated_string::Translation {
            text: value.to_string(),
            language: Some("en".into()),
        }],
    }
}

/// The unified entity the primary crate emits: one train, with its update and its position, under
/// its per-train-day id and with no vehicle descriptor.
fn train(now: u64, start_date: &str) -> FeedEntity {
    let trip = TripDescriptor {
        trip_id: Some("fixture-123".into()),
        route_id: Some("NEC".into()),
        start_date: Some(start_date.to_string()),
        ..Default::default()
    };
    // Track 7 boards through gate A, so it is its own platform stop.
    let platform = PLATFORM;
    FeedEntity {
        id: format!("{start_date}-123"),
        trip_update: Some(TripUpdate {
            trip: trip.clone(),
            timestamp: Some(now),
            stop_time_update: vec![
                // The stamped stop: stop_sequence set, stop_id cleared, platform assigned.
                StopTimeUpdate {
                    stop_sequence: Some(1),
                    departure: Some(StopTimeEvent {
                        delay: Some(0),
                        ..Default::default()
                    }),
                    stop_time_properties: Some(StopTimeProperties {
                        assigned_stop_id: Some(platform.to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                StopTimeUpdate {
                    stop_sequence: Some(2),
                    stop_id: Some("PHL".into()),
                    arrival: Some(StopTimeEvent {
                        delay: Some(120),
                        ..Default::default()
                    }),
                    departure: Some(StopTimeEvent {
                        delay: Some(120),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }),
        vehicle: Some(VehiclePosition {
            trip: Some(trip),
            position: Some(Position {
                latitude: 40.75,
                longitude: -73.99,
                bearing: Some(225.0),
                speed: Some(20.0),
                ..Default::default()
            }),
            timestamp: Some(now),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn advisory(now: u64) -> FeedEntity {
    FeedEntity {
        id: "fixture-advisory".into(),
        alert: Some(Alert {
            active_period: vec![TimeRange {
                start: Some(now.saturating_sub(3600)),
                end: Some(now + 86_400),
            }],
            informed_entity: vec![EntitySelector {
                route_id: Some("NEC".into()),
                ..Default::default()
            }],
            cause: Some(alert::Cause::Maintenance as i32),
            effect: Some(alert::Effect::SignificantDelays as i32),
            header_text: Some(text("Fixture advisory")),
            description_text: Some(text("A fixture advisory for the validators.")),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// `YYYYMMDD` for a Unix time in UTC (Howard Hinnant's days-to-civil algorithm).
fn yyyymmdd(unix: u64) -> String {
    let days = (unix / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}")
}

fn main() {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: validation_fixture OUT_DIR"),
    );
    std::fs::create_dir_all(&out).expect("create the output directory");

    let official = zip_of(STATIC_FILES);
    // New York: a parent station, platform 7 boarded through gate A, and platform 8.
    let table = PlatformTable::parse(PLATFORM_TABLE).expect("platform table");
    let combined = augment_static(&official, &table).expect("augment the fixture static feed");
    let parsed = Gtfs::from_reader(Cursor::new(combined.clone())).expect("parse the combined feed");
    for stop in ["NYP:station", PLATFORM, "NYP:track:8"] {
        assert!(
            parsed.stops.contains_key(stop),
            "missing platform stop {stop}"
        );
    }
    let version = format!("FIXTURE+tracks.{}", table.digest());
    let parsed = Arc::new(parsed);
    let snapshot = Arc::new(StaticSnapshot::identical(
        version,
        parsed.clone(),
        Arc::from(combined.clone()),
    ));

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after 1970")
        .as_secs();
    let start_date = yyyymmdd(now);
    let mut trip_updates = FeedMessage::default();
    let mut vehicle_positions = FeedMessage::default();
    let mut alerts = FeedMessage::default();
    let entity = train(now, &start_date);
    trip_updates.entity.push(entity.clone());
    vehicle_positions.entity.push(entity);
    alerts.entity.push(advisory(now));
    // The primary source's conversion sets each train's vehicle descriptor.
    let batch = batch_from_results(amtrak_gtfs_rt::GtfsAmtrakResults {
        trip_updates,
        vehicle_positions,
        alerts,
    });

    let candidate = GenerationBuilder::build(
        snapshot,
        SelectedBatch {
            source_name: "fixture",
            batch,
        },
        UNIX_EPOCH + std::time::Duration::from_secs(now),
    )
    .expect("build the fixture generation");
    // Every fixture entity must resolve against the fixture static feed, or the builder drops it.
    assert_eq!(
        candidate.trip_updates.entity.len(),
        1,
        "trip update was dropped"
    );
    assert_eq!(
        candidate.vehicle_positions.entity.len(),
        1,
        "vehicle was dropped"
    );
    assert_eq!(candidate.alerts.entity.len(), 1, "alert was dropped");
    // #100: the train's trip update and its position carry the same vehicle id, its per-train-day
    // id, and the train number as the label.
    let train_day = format!("{start_date}-123");
    let update_vehicle = candidate.trip_updates.entity[0]
        .trip_update
        .as_ref()
        .and_then(|update| update.vehicle.as_ref());
    let position_vehicle = candidate.vehicle_positions.entity[0]
        .vehicle
        .as_ref()
        .and_then(|position| position.vehicle.as_ref());
    for (product, vehicle) in [
        ("trip update", update_vehicle),
        ("vehicle position", position_vehicle),
    ] {
        let vehicle = vehicle.unwrap_or_else(|| panic!("the {product} has no vehicle descriptor"));
        assert_eq!(
            vehicle.id.as_deref(),
            Some(train_day.as_str()),
            "{product} vehicle.id"
        );
        assert_eq!(
            vehicle.label.as_deref(),
            Some("123"),
            "{product} vehicle.label"
        );
    }
    // Neither validator checks this: every stamped platform is a stop in the augmented feed, under
    // the same parent station as the stop it replaces.
    for entity in &candidate.trip_updates.entity {
        let update = entity.trip_update.as_ref().expect("trip update");
        for stop in &update.stop_time_update {
            let Some(assigned) = stop
                .stop_time_properties
                .as_ref()
                .and_then(|properties| properties.assigned_stop_id.as_deref())
            else {
                continue;
            };
            let platform = parsed
                .stops
                .get(assigned)
                .unwrap_or_else(|| panic!("assigned_stop_id {assigned} is not a stop"));
            assert_eq!(
                platform.parent_station.as_deref(),
                Some("NYP:station"),
                "platform {assigned} is not under the station"
            );
        }
    }
    let validated = CandidateValidator::validate(candidate).expect("validate the generation");

    for (name, bytes) in [
        ("official-static.zip", official.as_slice()),
        ("static.zip", combined.as_slice()),
        ("trip-updates.pb", validated.trip_updates.as_ref()),
        ("vehicle-positions.pb", validated.vehicle_positions.as_ref()),
        ("alerts.pb", validated.alerts.as_ref()),
    ] {
        std::fs::write(out.join(name), bytes).expect("write a fixture artifact");
    }
    println!(
        "fixture feed set written to {}: {} trip update(s), {} vehicle(s), {} alert(s)",
        out.display(),
        validated.entity_counts.trip_updates,
        validated.entity_counts.vehicle_positions,
        validated.entity_counts.alerts
    );
}
