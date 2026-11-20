use crate::sources::{RtBatch, RtSource, SourceError};
use amtrak_gtfs_rt::GtfsAmtrakResults;
use async_trait::async_trait;
use gtfs_realtime::FeedMessage;
use gtfs_structures::Gtfs;

/// Primary realtime source: delegates to catenary's `amtrak-gtfs-rt` crate,
/// which fetches and decrypts Amtrak's `getTrainsData`, matches trains to GTFS
/// trips (handling multi-day date offsets), and returns ready-made FeedMessages.
pub struct AmtrakSource {
    client: reqwest::Client,
}

impl AmtrakSource {
    pub fn new() -> AmtrakSource {
        AmtrakSource {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for AmtrakSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RtSource for AmtrakSource {
    fn name(&self) -> &'static str {
        "amtrak"
    }

    async fn fetch(&self, gtfs: &Gtfs) -> Result<RtBatch, SourceError> {
        let results = amtrak_gtfs_rt::fetch_amtrak_gtfs_rt(gtfs, &self.client).await?;
        Ok(batch_from_results(results))
    }
}

/// The batch for the crate's `results`, with each train's vehicle descriptor set on its trip update
/// and its vehicle position (`vehicle.id` the entity's per-train-day id, `vehicle.label` the train
/// number; see [`crate::sources::identity`]). The crate itself sets no descriptor.
///
/// An answer with no trip updates and no vehicle positions is empty even when it carries alerts:
/// the crate adds Pacific Surfliner advisories whether or not Amtrak returned any trains, and an
/// alerts-only batch would stop the chain from falling back (#110).
pub fn batch_from_results(results: GtfsAmtrakResults) -> RtBatch {
    if results.trip_updates.entity.is_empty() && results.vehicle_positions.entity.is_empty() {
        return RtBatch {
            trip_updates: FeedMessage::default(),
            vehicle_positions: FeedMessage::default(),
            alerts: FeedMessage::default(),
        };
    }
    let mut batch = RtBatch {
        trip_updates: results.trip_updates,
        vehicle_positions: results.vehicle_positions,
        alerts: results.alerts,
    };
    crate::sources::identity::identify_batch(&mut batch);
    batch
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtfs_realtime::{Alert, FeedEntity};

    fn results(trains: usize, alerts: usize) -> GtfsAmtrakResults {
        let feed = |ids: Vec<String>, alert: bool| FeedMessage {
            entity: ids
                .into_iter()
                .map(|id| FeedEntity {
                    id,
                    alert: alert.then(Alert::default),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let trains: Vec<String> = (0..trains).map(|i| format!("train-{i}")).collect();
        GtfsAmtrakResults {
            trip_updates: feed(trains.clone(), false),
            vehicle_positions: feed(trains, false),
            alerts: feed(
                (0..alerts).map(|i| format!("surfliner-{i}")).collect(),
                true,
            ),
        }
    }

    /// #110: the crate adds Pacific Surfliner advisories even when Amtrak returns no trains. Such
    /// an answer must still be empty, so the chain falls back instead of publishing alerts only.
    #[test]
    fn an_answer_with_only_alerts_is_empty() {
        assert!(batch_from_results(results(0, 2)).is_empty());
    }

    #[test]
    fn an_answer_with_trains_keeps_its_alerts() {
        let batch = batch_from_results(results(1, 2));
        assert_eq!(batch.trip_updates.entity.len(), 1);
        assert_eq!(batch.vehicle_positions.entity.len(), 1);
        assert_eq!(batch.alerts.entity.len(), 2);
    }

    /// #100: one train, as the crate encodes it (one unified entity copied into each product, no
    /// descriptor), carries the same `vehicle.id` on its trip update and its vehicle position, and
    /// the builder keeps both.
    #[test]
    fn both_products_carry_the_same_vehicle_id_for_one_train() {
        use crate::orchestrator::{GenerationBuilder, SelectedBatch, StaticSnapshot};
        use gtfs_realtime::trip_update::{StopTimeEvent, StopTimeUpdate};
        use gtfs_realtime::{
            FeedEntity, FeedMessage, Position, TripDescriptor, TripUpdate, VehiclePosition,
        };
        use std::io::{Cursor, Write};
        use std::sync::Arc;
        use std::time::{Duration, UNIX_EPOCH};

        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, contents) in [
            ("agency.txt", "agency_id,agency_name,agency_url,agency_timezone\na,Amtrak,https://amtrak.com,America/New_York\n"),
            ("stops.txt", "stop_id,stop_name,stop_lat,stop_lon\nNYP,New York,40.75,-73.99\n"),
            ("routes.txt", "route_id,agency_id,route_short_name,route_long_name,route_type\nr,a,NER,Northeast Regional,2\n"),
            ("trips.txt", "route_id,service_id,trip_id,trip_short_name\nr,svc,320135,172\n"),
            ("stop_times.txt", "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n320135,10:00:00,10:00:00,NYP,1\n"),
            ("calendar_dates.txt", "service_id,date,exception_type\nsvc,20261119,1\n"),
        ] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(contents.as_bytes()).unwrap();
        }
        let zip = archive.finish().unwrap().into_inner();
        let gtfs = Gtfs::from_reader(Cursor::new(zip.clone())).unwrap();

        let trip = TripDescriptor {
            trip_id: Some("320135".into()),
            route_id: Some("r".into()),
            start_date: Some("20261119".into()),
            ..Default::default()
        };
        let unified = FeedEntity {
            id: "20261119-172".into(),
            trip_update: Some(TripUpdate {
                trip: trip.clone(),
                stop_time_update: vec![StopTimeUpdate {
                    stop_sequence: Some(1),
                    stop_id: Some("NYP".into()),
                    departure: Some(StopTimeEvent {
                        delay: Some(60),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            vehicle: Some(VehiclePosition {
                trip: Some(trip),
                position: Some(Position {
                    latitude: 40.75,
                    longitude: -73.99,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let message = FeedMessage {
            entity: vec![unified],
            ..Default::default()
        };
        let batch = batch_from_results(amtrak_gtfs_rt::GtfsAmtrakResults {
            trip_updates: message.clone(),
            vehicle_positions: message,
            alerts: FeedMessage::default(),
        });
        let candidate = GenerationBuilder::build(
            Arc::new(StaticSnapshot::identical(
                "STATIC-V1",
                Arc::new(gtfs),
                Arc::from(zip),
            )),
            SelectedBatch {
                source_name: "amtrak",
                batch,
            },
            UNIX_EPOCH + Duration::from_secs(1_800_000_000),
        )
        .unwrap();
        let update = candidate.trip_updates.entity[0]
            .trip_update
            .as_ref()
            .unwrap();
        let position = candidate.vehicle_positions.entity[0]
            .vehicle
            .as_ref()
            .unwrap();
        for vehicle in [update.vehicle.as_ref(), position.vehicle.as_ref()] {
            let vehicle = vehicle.expect("a vehicle descriptor");
            assert_eq!(vehicle.id.as_deref(), Some("20261119-172"));
            assert_eq!(vehicle.label.as_deref(), Some("172"));
        }
    }

    #[test]
    fn source_is_named_amtrak() {
        let src = AmtrakSource::new();
        assert_eq!(src.name(), "amtrak");
    }

    // Live test: hits Amtrak's real endpoints. Run explicitly with:
    //   cargo test sources::amtrak -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_fetch_returns_batch() {
        tokio::time::timeout(std::time::Duration::from_secs(180), async {
            let gtfs = Gtfs::from_url_async("https://content.amtrak.com/content/gtfs/GTFS.zip")
                .await
                .unwrap();
            let src = AmtrakSource::new();
            let batch = src.fetch(&gtfs).await.unwrap();
            // At virtually any hour some Amtrak trains are running.
            assert!(
                !batch.trip_updates.entity.is_empty(),
                "expected live trip updates"
            );
            assert!(
                !batch.vehicle_positions.entity.is_empty(),
                "expected live vehicle positions"
            );
            println!(
                "Amtrak source: trip_updates={}, vehicle_positions={}, alerts={}",
                batch.trip_updates.entity.len(),
                batch.vehicle_positions.entity.len(),
                batch.alerts.entity.len()
            );
        })
        .await
        .expect("live Amtrak fetch exceeded 180 seconds");
    }
}
