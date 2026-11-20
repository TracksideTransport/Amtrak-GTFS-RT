//! An explicit `schedule_relationship` on every trip and stop time update (#101).
//!
//! Neither source sets `schedule_relationship` on a scheduled trip or stop, and GTFS-Realtime reads
//! an absent value as `SCHEDULED`. The best practices ask for it to be written out, and the
//! MobilityData realtime validator reports W009 when it is not. The generation builder calls
//! [`make_explicit`] on each product after every source and rewrite (the fallback, track stamping,
//! notices, the alert header fill) has run, so one place covers them all.
//!
//! An unset stop of an `UNSCHEDULED` trip is filled with `UNSCHEDULED`, because the reference
//! requires every stop of such a trip to be `UNSCHEDULED`.
//!
//! Only an unset value is filled. `SKIPPED`, `NO_DATA`, `CANCELED`, `DELETED` and any other value a
//! source set is kept. No descriptor is added where there is none: an alert selector without a
//! `trip` and a vehicle position without one stay as they are.

use gtfs_realtime::{trip_descriptor, trip_update::stop_time_update, FeedMessage, TripDescriptor};

/// Sets `SCHEDULED` where `schedule_relationship` is unset on the trip descriptor of every trip
/// update, vehicle position and alert informed entity in `message`, and on every stop time update.
/// An unset stop on a trip whose relationship is `UNSCHEDULED` gets `UNSCHEDULED` instead, as the
/// reference requires for every stop of such a trip.
pub fn make_explicit(message: &mut FeedMessage) {
    for entity in &mut message.entity {
        if let Some(update) = entity.trip_update.as_mut() {
            schedule(&mut update.trip);
            // The reference: an UNSCHEDULED trip must set all its stops to UNSCHEDULED.
            let stop_default = if update.trip.schedule_relationship
                == Some(trip_descriptor::ScheduleRelationship::Unscheduled as i32)
            {
                stop_time_update::ScheduleRelationship::Unscheduled
            } else {
                stop_time_update::ScheduleRelationship::Scheduled
            };
            for stop in &mut update.stop_time_update {
                stop.schedule_relationship
                    .get_or_insert(stop_default as i32);
            }
        }
        if let Some(trip) = entity
            .vehicle
            .as_mut()
            .and_then(|position| position.trip.as_mut())
        {
            schedule(trip);
        }
        if let Some(alert) = entity.alert.as_mut() {
            for trip in alert
                .informed_entity
                .iter_mut()
                .filter_map(|selector| selector.trip.as_mut())
            {
                schedule(trip);
            }
        }
    }
}

fn schedule(trip: &mut TripDescriptor) {
    trip.schedule_relationship
        .get_or_insert(trip_descriptor::ScheduleRelationship::Scheduled as i32);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtfs_realtime::trip_update::StopTimeUpdate;
    use gtfs_realtime::{Alert, EntitySelector, FeedEntity, TripUpdate, VehiclePosition};

    const SCHEDULED: i32 = trip_descriptor::ScheduleRelationship::Scheduled as i32;
    const STOP_SCHEDULED: i32 = stop_time_update::ScheduleRelationship::Scheduled as i32;

    fn trip(relationship: Option<trip_descriptor::ScheduleRelationship>) -> TripDescriptor {
        TripDescriptor {
            trip_id: Some("trip".into()),
            schedule_relationship: relationship.map(|value| value as i32),
            ..Default::default()
        }
    }

    fn stop(relationship: Option<stop_time_update::ScheduleRelationship>) -> StopTimeUpdate {
        StopTimeUpdate {
            stop_sequence: Some(1),
            schedule_relationship: relationship.map(|value| value as i32),
            ..Default::default()
        }
    }

    fn message(entities: Vec<FeedEntity>) -> FeedMessage {
        FeedMessage {
            entity: entities,
            ..Default::default()
        }
    }

    fn update_of(entity: &FeedEntity) -> &TripUpdate {
        entity.trip_update.as_ref().unwrap()
    }

    #[test]
    fn unset_trips_and_stops_become_scheduled_in_every_product() {
        let mut message = message(vec![
            FeedEntity {
                id: "update".into(),
                trip_update: Some(TripUpdate {
                    trip: trip(None),
                    stop_time_update: vec![stop(None), stop(None)],
                    ..Default::default()
                }),
                ..Default::default()
            },
            FeedEntity {
                id: "position".into(),
                vehicle: Some(VehiclePosition {
                    trip: Some(trip(None)),
                    ..Default::default()
                }),
                ..Default::default()
            },
            FeedEntity {
                id: "alert".into(),
                alert: Some(Alert {
                    informed_entity: vec![
                        EntitySelector {
                            route_id: Some("route".into()),
                            trip: Some(trip(None)),
                            ..Default::default()
                        },
                        EntitySelector {
                            trip: Some(trip(None)),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ]);
        make_explicit(&mut message);

        let update = update_of(&message.entity[0]);
        assert_eq!(update.trip.schedule_relationship, Some(SCHEDULED));
        for stop in &update.stop_time_update {
            assert_eq!(stop.schedule_relationship, Some(STOP_SCHEDULED));
        }
        let position = message.entity[1].vehicle.as_ref().unwrap();
        assert_eq!(
            position.trip.as_ref().unwrap().schedule_relationship,
            Some(SCHEDULED)
        );
        let alert = message.entity[2].alert.as_ref().unwrap();
        for selector in &alert.informed_entity {
            assert_eq!(
                selector.trip.as_ref().unwrap().schedule_relationship,
                Some(SCHEDULED)
            );
        }
    }

    #[test]
    fn values_a_source_set_are_kept() {
        use stop_time_update::ScheduleRelationship as Stop;
        use trip_descriptor::ScheduleRelationship as Trip;

        let mut entities = Vec::new();
        for (index, relationship) in [
            Trip::Canceled,
            Trip::Deleted,
            Trip::Unscheduled,
            Trip::Duplicated,
            Trip::New,
            Trip::Replacement,
        ]
        .into_iter()
        .enumerate()
        {
            entities.push(FeedEntity {
                id: format!("update-{index}"),
                trip_update: Some(TripUpdate {
                    trip: trip(Some(relationship)),
                    stop_time_update: vec![
                        stop(Some(Stop::Skipped)),
                        stop(Some(Stop::NoData)),
                        stop(Some(Stop::Unscheduled)),
                        stop(None),
                    ],
                    ..Default::default()
                }),
                vehicle: Some(VehiclePosition {
                    trip: Some(trip(Some(relationship))),
                    ..Default::default()
                }),
                alert: Some(Alert {
                    informed_entity: vec![EntitySelector {
                        trip: Some(trip(Some(relationship))),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
        let original = message(entities);
        let mut filled = original.clone();
        make_explicit(&mut filled);

        for (before, after) in original.entity.iter().zip(&filled.entity) {
            let relationship = update_of(before).trip.schedule_relationship;
            assert_eq!(update_of(after).trip.schedule_relationship, relationship);
            assert_eq!(
                after
                    .vehicle
                    .as_ref()
                    .unwrap()
                    .trip
                    .as_ref()
                    .unwrap()
                    .schedule_relationship,
                relationship
            );
            assert_eq!(
                after.alert.as_ref().unwrap().informed_entity[0]
                    .trip
                    .as_ref()
                    .unwrap()
                    .schedule_relationship,
                relationship
            );
            let stops: Vec<_> = update_of(after)
                .stop_time_update
                .iter()
                .map(|stop| stop.schedule_relationship)
                .collect();
            assert_eq!(
                stops,
                vec![
                    Some(Stop::Skipped as i32),
                    Some(Stop::NoData as i32),
                    Some(Stop::Unscheduled as i32),
                    // An unset stop follows its trip: UNSCHEDULED under an UNSCHEDULED trip.
                    Some(if relationship == Some(Trip::Unscheduled as i32) {
                        Stop::Unscheduled as i32
                    } else {
                        STOP_SCHEDULED
                    }),
                ]
            );
        }
    }

    /// The reference: a trip with `schedule_relationship: UNSCHEDULED` must set all its stop time
    /// updates to `UNSCHEDULED`. An unset stop on such a trip becomes `UNSCHEDULED`, not
    /// `SCHEDULED`, and a stop value a source set is still kept.
    #[test]
    fn an_unset_stop_on_an_unscheduled_trip_becomes_unscheduled() {
        use stop_time_update::ScheduleRelationship as Stop;
        use trip_descriptor::ScheduleRelationship as Trip;

        let mut message = message(vec![FeedEntity {
            id: "unscheduled".into(),
            trip_update: Some(TripUpdate {
                trip: trip(Some(Trip::Unscheduled)),
                stop_time_update: vec![stop(None), stop(Some(Stop::Skipped)), stop(None)],
                ..Default::default()
            }),
            ..Default::default()
        }]);
        make_explicit(&mut message);
        let update = update_of(&message.entity[0]);
        assert_eq!(
            update.trip.schedule_relationship,
            Some(Trip::Unscheduled as i32)
        );
        let stops: Vec<_> = update
            .stop_time_update
            .iter()
            .map(|stop| stop.schedule_relationship)
            .collect();
        assert_eq!(
            stops,
            vec![
                Some(Stop::Unscheduled as i32),
                Some(Stop::Skipped as i32),
                Some(Stop::Unscheduled as i32),
            ]
        );
    }

    #[test]
    fn no_trip_descriptor_is_invented() {
        let mut message = message(vec![
            FeedEntity {
                id: "position".into(),
                vehicle: Some(VehiclePosition::default()),
                ..Default::default()
            },
            FeedEntity {
                id: "notice".into(),
                alert: Some(Alert {
                    informed_entity: vec![EntitySelector {
                        stop_id: Some("BOS".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ]);
        let original = message.clone();
        make_explicit(&mut message);
        assert_eq!(message, original);
    }
}
