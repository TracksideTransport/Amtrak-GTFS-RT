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

/// Converts the crate's answer into a batch. An answer with no trip updates and no vehicle
/// positions is empty even when it carries alerts: the crate adds Pacific Surfliner advisories
/// whether or not Amtrak returned any trains, and an alerts-only batch would stop the chain from
/// falling back (#110).
fn batch_from_results(results: GtfsAmtrakResults) -> RtBatch {
    if results.trip_updates.entity.is_empty() && results.vehicle_positions.entity.is_empty() {
        return RtBatch {
            trip_updates: FeedMessage::default(),
            vehicle_positions: FeedMessage::default(),
            alerts: FeedMessage::default(),
        };
    }
    RtBatch {
        trip_updates: results.trip_updates,
        vehicle_positions: results.vehicle_positions,
        alerts: results.alerts,
    }
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
