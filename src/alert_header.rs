//! Fills `header_text` on an alert that arrives without one.
//!
//! Invariant: every alert the generation builder publishes carries a non-empty `header_text`,
//! which the GTFS-Realtime reference marks Required. The train-status alerts from the
//! `amtrak-gtfs-rt` crate (TransitDocs ASM notices attached to one train) carry only
//! `description_text`, such as `Delay Notification: As of 11:40 PM ET Train 98 is stopped ...`.
//! Their header is the description's leading label, after the train number when the informed
//! trip names one: `Train 98: Delay Notification`. Without a label it is `Train 98 service alert`,
//! or `Service alert` when no train is known. An alert that already has a header (the station and
//! passenger notices, the Pacific Surfliner advisories) is left as it is.
//!
//! `effect` is set only where the label leaves no doubt: when every notice in the description is
//! a `Delay Notification`, the effect is `SIGNIFICANT_DELAYS`. Any other or mixed label leaves
//! `effect` unset.

use gtfs_realtime::translated_string::Translation;
use gtfs_realtime::{alert, Alert, TranslatedString};
use gtfs_structures::Gtfs;

/// Longest leading label read as a title. Longer text before a colon is a sentence.
const MAX_LABEL_CHARS: usize = 48;

/// Most words in a leading label.
const MAX_LABEL_WORDS: usize = 5;

/// The label of a notice about a train running late.
const DELAY_LABEL: &str = "Delay Notification";

/// Gives `alert` a `header_text` when it has none, and an `effect` when its notices are all delay
/// notifications and it has no effect yet. `gtfs` resolves the informed trip's train number.
pub fn ensure_header_text(alert: &mut Alert, gtfs: &Gtfs) {
    if has_text(alert.header_text.as_ref()) {
        return;
    }
    let description = alert
        .description_text
        .as_ref()
        .and_then(preferred_translation);
    let text = description.map_or("", |translation| translation.text.as_str());
    let language = description.and_then(|translation| translation.language.clone());
    let notices: Vec<&str> = text
        .split("\n\n")
        .map(str::trim)
        .filter(|notice| !notice.is_empty())
        .collect();
    let labels: Vec<&str> = notices.iter().filter_map(|notice| label(notice)).collect();
    let header = match (train_number(alert, gtfs), labels.first()) {
        (Some(train), Some(label)) => format!("Train {train}: {label}"),
        (None, Some(label)) => (*label).to_string(),
        (Some(train), None) => format!("Train {train} service alert"),
        (None, None) => "Service alert".to_string(),
    };
    alert.header_text = Some(TranslatedString {
        translation: vec![Translation {
            text: header,
            language,
        }],
    });
    let all_delays = !notices.is_empty()
        && labels.len() == notices.len()
        && labels
            .iter()
            .all(|label| label.eq_ignore_ascii_case(DELAY_LABEL));
    if alert.effect.is_none() && all_delays {
        alert.set_effect(alert::Effect::SignificantDelays);
    }
}

fn has_text(text: Option<&TranslatedString>) -> bool {
    text.is_some_and(|text| {
        text.translation
            .iter()
            .any(|translation| !translation.text.trim().is_empty())
    })
}

/// The English translation when there is one, else the first non-empty one.
fn preferred_translation(text: &TranslatedString) -> Option<&Translation> {
    let non_empty = || {
        text.translation
            .iter()
            .filter(|translation| !translation.text.trim().is_empty())
    };
    non_empty()
        .find(|translation| {
            translation
                .language
                .as_deref()
                .is_some_and(|language| language.eq_ignore_ascii_case("en"))
        })
        .or_else(|| non_empty().next())
}

/// The title before the first colon of a notice's first line, such as `Delay Notification` or
/// `Service Resumption`. Text with digits or punctuation (`As of 11:40 PM`) is not a title.
fn label(notice: &str) -> Option<&str> {
    let first_line = notice.lines().next()?;
    let (candidate, _) = first_line.split_once(':')?;
    let candidate = candidate.trim();
    let words = candidate.split_whitespace().count();
    let starts_with_letter = candidate
        .chars()
        .next()
        .is_some_and(|first| first.is_alphabetic());
    let title_characters = candidate
        .chars()
        .all(|character| character.is_alphabetic() || " -&/'".contains(character));
    (starts_with_letter
        && title_characters
        && candidate.chars().count() <= MAX_LABEL_CHARS
        && (1..=MAX_LABEL_WORDS).contains(&words))
    .then_some(candidate)
}

/// The train number (`trip_short_name`) of the first informed trip that the static feed names.
fn train_number<'a>(alert: &Alert, gtfs: &'a Gtfs) -> Option<&'a str> {
    alert
        .informed_entity
        .iter()
        .filter_map(|selector| selector.trip.as_ref()?.trip_id.as_deref())
        .filter_map(|trip_id| gtfs.trips.get(trip_id)?.trip_short_name.as_deref())
        .map(str::trim)
        .find(|number| !number.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtfs_realtime::{EntitySelector, TripDescriptor};
    use gtfs_structures::Trip;

    fn gtfs() -> Gtfs {
        let mut gtfs = Gtfs::default();
        gtfs.trips.insert(
            "318074".into(),
            Trip {
                id: "318074".into(),
                trip_short_name: Some("98".into()),
                ..Default::default()
            },
        );
        gtfs
    }

    fn text(value: &str, language: Option<&str>) -> Option<TranslatedString> {
        Some(TranslatedString {
            translation: vec![Translation {
                text: value.into(),
                language: language.map(str::to_string),
            }],
        })
    }

    fn train_alert(description: &str, trip_id: &str) -> Alert {
        Alert {
            informed_entity: vec![EntitySelector {
                route_id: Some("42947".into()),
                trip: Some(TripDescriptor {
                    trip_id: Some(trip_id.into()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            description_text: text(description, Some("en")),
            ..Default::default()
        }
    }

    fn header(alert: &Alert) -> (&str, Option<&str>) {
        let translation = &alert.header_text.as_ref().unwrap().translation[0];
        (translation.text.as_str(), translation.language.as_deref())
    }

    /// #107: the train-status alert from the issue gets a header and a delay effect.
    #[test]
    fn a_delay_notification_gets_a_train_header_and_significant_delays() {
        let mut alert = train_alert(
            "Delay Notification: As of 11:40 PM ET Train 98 is stopped north of Kingstree (KTR) \
             due to freight interference.",
            "318074",
        );
        ensure_header_text(&mut alert, &gtfs());
        assert_eq!(header(&alert), ("Train 98: Delay Notification", Some("en")));
        assert_eq!(alert.effect(), alert::Effect::SignificantDelays);
    }

    #[test]
    fn other_labels_title_the_alert_but_leave_the_effect_unset() {
        for (description, expected) in [
            (
                "Service Resumption: Train 98 is moving again.",
                "Train 98: Service Resumption",
            ),
            (
                "Travel Update: Buses replace Train 98 north of Richmond.",
                "Train 98: Travel Update",
            ),
            (
                "Delay Notification: Train 98 is 40 minutes late.\n\nService Resumption: \
                 Train 98 is moving again.",
                "Train 98: Delay Notification",
            ),
        ] {
            let mut alert = train_alert(description, "318074");
            ensure_header_text(&mut alert, &gtfs());
            assert_eq!(header(&alert).0, expected, "{description}");
            assert_eq!(alert.effect, None, "{description}");
        }
    }

    #[test]
    fn every_delay_notice_in_a_joined_description_keeps_the_delay_effect() {
        let mut alert = train_alert(
            "Delay Notification: Train 98 is 40 minutes late.\n\nDelay Notification: Train 98 \
             is now 60 minutes late.",
            "318074",
        );
        ensure_header_text(&mut alert, &gtfs());
        assert_eq!(alert.effect(), alert::Effect::SignificantDelays);
    }

    #[test]
    fn a_description_without_a_title_falls_back_to_the_train_or_a_generic_header() {
        let mut alert = train_alert("As of 11:40 PM ET Train 98 is stopped.", "318074");
        ensure_header_text(&mut alert, &gtfs());
        assert_eq!(header(&alert).0, "Train 98 service alert");
        assert_eq!(alert.effect, None);

        let mut unknown_trip = train_alert("As of 11:40 PM ET the train is stopped.", "missing");
        ensure_header_text(&mut unknown_trip, &gtfs());
        assert_eq!(header(&unknown_trip).0, "Service alert");

        let mut label_only = train_alert("Delay Notification: running late.", "missing");
        ensure_header_text(&mut label_only, &gtfs());
        assert_eq!(header(&label_only).0, "Delay Notification");
    }

    #[test]
    fn sentence_like_text_before_a_colon_is_not_a_title() {
        assert_eq!(label("As of 11:40 PM ET the train is late."), None);
        assert_eq!(
            label("Please note that the following trains are affected today: 98, 91"),
            None
        );
        assert_eq!(
            label("Delay Notification: late"),
            Some("Delay Notification")
        );
        assert_eq!(label("No colon here"), None);
    }

    #[test]
    fn an_existing_header_and_effect_are_kept() {
        let mut alert = train_alert("Delay Notification: Train 98 is late.", "318074");
        alert.header_text = text("Boston South Station Restroom Improvement Project", None);
        let before = alert.clone();
        ensure_header_text(&mut alert, &gtfs());
        assert_eq!(alert, before);

        let mut with_effect = train_alert("Delay Notification: Train 98 is late.", "318074");
        with_effect.set_effect(alert::Effect::Detour);
        ensure_header_text(&mut with_effect, &gtfs());
        assert_eq!(with_effect.effect(), alert::Effect::Detour);
    }

    #[test]
    fn the_english_description_is_preferred_and_its_language_is_kept() {
        let mut alert = train_alert("", "318074");
        alert.description_text = Some(TranslatedString {
            translation: vec![
                Translation {
                    text: "Aviso de retraso: el tren 98 va tarde.".into(),
                    language: Some("es".into()),
                },
                Translation {
                    text: "Delay Notification: Train 98 is late.".into(),
                    language: Some("EN".into()),
                },
            ],
        });
        ensure_header_text(&mut alert, &gtfs());
        assert_eq!(header(&alert), ("Train 98: Delay Notification", Some("EN")));
    }
}
