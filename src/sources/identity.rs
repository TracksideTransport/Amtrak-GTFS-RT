//! One vehicle identity per train, whichever source built the generation (#100, #122).
//!
//! A train's trip update and its vehicle position both carry the same `VehicleDescriptor`:
//! `vehicle.id` is the per-train-day id `{YYYYMMDD}-{number}`, the entity id the primary crate
//! already gives the train (`20261119-172`), and `vehicle.label` is the train number (`172`). The
//! fallback source uses the same id for its entities. A consumer can join a trip update to its
//! position by `vehicle.id`, and a train keeps its identity when the source changes.
//!
//! An entity whose train number or service date cannot be read gets no descriptor, rather than an
//! invented one.

use crate::sources::RtBatch;
use chrono::NaiveDate;
use gtfs_realtime::{FeedMessage, VehicleDescriptor};

/// The per-train-day id for train `number` on service date `service_date` (`YYYYMMDD`).
pub fn train_day_id(service_date: &str, number: &str) -> String {
    format!("{service_date}-{number}")
}

/// The descriptor both products carry for train `number` on `service_date` (`YYYYMMDD`).
pub fn descriptor(service_date: &str, number: &str) -> VehicleDescriptor {
    VehicleDescriptor {
        id: Some(train_day_id(service_date, number)),
        label: Some(number.to_string()),
        ..Default::default()
    }
}

/// Sets the vehicle descriptor on every trip update and vehicle position in `batch` whose entity
/// id is a per-train-day id. Other descriptor fields are kept; other entities are left alone.
pub fn identify_batch(batch: &mut RtBatch) {
    identify(&mut batch.trip_updates);
    identify(&mut batch.vehicle_positions);
    identify(&mut batch.alerts);
}

/// [`identify_batch`] for one product.
pub fn identify(message: &mut FeedMessage) {
    for entity in &mut message.entity {
        let Some((_, number)) = parse_train_day_id(&entity.id) else {
            continue;
        };
        let stamp = |vehicle: &mut Option<VehicleDescriptor>| {
            let vehicle = vehicle.get_or_insert_with(VehicleDescriptor::default);
            vehicle.id = Some(entity.id.clone());
            vehicle.label = Some(number.to_string());
        };
        if let Some(update) = entity.trip_update.as_mut() {
            stamp(&mut update.vehicle);
        }
        if let Some(position) = entity.vehicle.as_mut() {
            stamp(&mut position.vehicle);
        }
    }
}

/// The service date and train number in a per-train-day id, or `None` when `id` is not one: the
/// date must be a calendar date written `YYYYMMDD`, and the number non-empty ASCII letters and
/// digits.
fn parse_train_day_id(id: &str) -> Option<(&str, &str)> {
    let (date, number) = id.split_once('-')?;
    let is_date = date.len() == 8
        && date.bytes().all(|byte| byte.is_ascii_digit())
        && NaiveDate::parse_from_str(date, "%Y%m%d").is_ok();
    let is_number = !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_alphanumeric());
    (is_date && is_number).then_some((date, number))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtfs_realtime::{FeedEntity, Position, TripDescriptor, TripUpdate, VehiclePosition};

    /// One train as the primary crate encodes it: one unified entity, no descriptor.
    fn unified(id: &str) -> FeedEntity {
        let trip = TripDescriptor {
            trip_id: Some("320135".into()),
            start_date: Some("20261119".into()),
            ..Default::default()
        };
        FeedEntity {
            id: id.into(),
            trip_update: Some(TripUpdate {
                trip: trip.clone(),
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
        }
    }

    fn batch_of(entities: Vec<FeedEntity>) -> RtBatch {
        let message = FeedMessage {
            entity: entities,
            ..Default::default()
        };
        RtBatch {
            trip_updates: message.clone(),
            vehicle_positions: message,
            alerts: FeedMessage::default(),
        }
    }

    fn ids(descriptor: Option<&VehicleDescriptor>) -> (Option<&str>, Option<&str>) {
        descriptor.map_or((None, None), |vehicle| {
            (vehicle.id.as_deref(), vehicle.label.as_deref())
        })
    }

    #[test]
    fn both_products_carry_the_train_day_id_and_the_number() {
        let mut batch = batch_of(vec![unified("20261119-172")]);
        identify_batch(&mut batch);
        let expected = (Some("20261119-172"), Some("172"));
        let update = &batch.trip_updates.entity[0];
        let position = &batch.vehicle_positions.entity[0];
        assert_eq!(
            ids(update.trip_update.as_ref().unwrap().vehicle.as_ref()),
            expected
        );
        assert_eq!(
            ids(position.vehicle.as_ref().unwrap().vehicle.as_ref()),
            expected
        );
    }

    #[test]
    fn other_descriptor_fields_are_kept() {
        let mut batch = batch_of(vec![unified("20261119-172")]);
        batch.vehicle_positions.entity[0]
            .vehicle
            .as_mut()
            .unwrap()
            .vehicle = Some(VehicleDescriptor {
            label: Some("Northeast Regional".into()),
            license_plate: Some("kept".into()),
            ..Default::default()
        });
        identify_batch(&mut batch);
        let vehicle = batch.vehicle_positions.entity[0]
            .vehicle
            .as_ref()
            .unwrap()
            .vehicle
            .as_ref()
            .unwrap();
        assert_eq!(vehicle.id.as_deref(), Some("20261119-172"));
        assert_eq!(vehicle.label.as_deref(), Some("172"));
        assert_eq!(vehicle.license_plate.as_deref(), Some("kept"));
    }

    #[test]
    fn an_id_without_a_date_and_number_gets_no_descriptor() {
        for id in [
            "PAC_SURF_ID",
            "172",
            "20261119",
            "20261119-",
            "2026111-172",
            "20261340-172",
            "-172",
            "2026111x-172",
            "20261119-17 2",
        ] {
            let mut batch = batch_of(vec![unified(id)]);
            identify_batch(&mut batch);
            let entity = &batch.trip_updates.entity[0];
            assert!(
                entity.trip_update.as_ref().unwrap().vehicle.is_none(),
                "{id}"
            );
            assert!(
                batch.vehicle_positions.entity[0]
                    .vehicle
                    .as_ref()
                    .unwrap()
                    .vehicle
                    .is_none(),
                "{id}"
            );
        }
    }

    #[test]
    fn the_descriptor_is_the_train_day_id_and_number() {
        let vehicle = descriptor("20261119", "172");
        assert_eq!(vehicle.id.as_deref(), Some("20261119-172"));
        assert_eq!(vehicle.label.as_deref(), Some("172"));
        assert_eq!(train_day_id("20261119", "172"), "20261119-172");
    }
}
