use async_trait::async_trait;
use gtfs_realtime::FeedMessage;
use gtfs_structures::Gtfs;

pub type SourceError = Box<dyn std::error::Error + Send + Sync>;

/// Longest source-error description put in a log field.
const MAX_ERROR_CHARS: usize = 300;

/// A one-line description of a source error and its causes, safe for a log field.
///
/// `reqwest` names the request URL in its message (`error sending request for url (...)`); that
/// part is cut, and any other word that contains `://` becomes `<url>`, so neither an upstream URL
/// nor a configured one with credentials reaches a log line. The text is capped at 300 characters.
pub fn describe_error(error: &(dyn std::error::Error + 'static)) -> String {
    let mut parts = Vec::new();
    let mut next = Some(error);
    while let Some(current) = next {
        let mut text = current.to_string();
        if let Some(url) = current
            .downcast_ref::<reqwest::Error>()
            .and_then(reqwest::Error::url)
        {
            text = text.replace(&format!(" for url ({url})"), "");
        }
        parts.push(
            text.split(' ')
                .map(|word| if word.contains("://") { "<url>" } else { word })
                .collect::<Vec<_>>()
                .join(" "),
        );
        next = current.source();
    }
    let joined = parts.join(": ");
    match joined.char_indices().nth(MAX_ERROR_CHARS) {
        Some((cut, _)) => format!("{}...", &joined[..cut]),
        None => joined,
    }
}

pub mod advisories;
#[cfg(test)]
mod advisories_live_tests;
pub mod amtrak;
pub mod asm;
pub mod tracks;

#[derive(Clone, Debug)]
pub struct RtBatch {
    pub trip_updates: FeedMessage,
    pub vehicle_positions: FeedMessage,
    pub alerts: FeedMessage,
}

impl RtBatch {
    #[cfg(test)]
    pub fn empty() -> RtBatch {
        RtBatch {
            trip_updates: FeedMessage::default(),
            vehicle_positions: FeedMessage::default(),
            alerts: FeedMessage::default(),
        }
    }

    /// A batch is empty when no source produced any entities. The orchestrator
    /// treats an empty batch as "no fresh data" and advances to the next source.
    pub fn is_empty(&self) -> bool {
        self.trip_updates.entity.is_empty()
            && self.vehicle_positions.entity.is_empty()
            && self.alerts.entity.is_empty()
    }
}

/// A realtime data source. Implementations normalize their provider's data into
/// an `RtBatch` so the orchestrator never depends on which provider produced it.
#[async_trait]
pub trait RtSource: Send + Sync {
    fn name(&self) -> &'static str;
    async fn fetch(&self, gtfs: &Gtfs) -> Result<RtBatch, SourceError>;
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use gtfs_realtime::{FeedEntity, FeedMessage};

    #[expect(
        clippy::large_enum_variant,
        reason = "test fixture ergonomics are clearer with an inline successful batch"
    )]
    pub enum Behavior {
        Ok(RtBatch),
        Empty,
        Fail,
    }

    pub struct MockSource {
        pub name: &'static str,
        pub behavior: Behavior,
    }

    #[async_trait]
    impl RtSource for MockSource {
        fn name(&self) -> &'static str {
            self.name
        }
        async fn fetch(&self, _gtfs: &Gtfs) -> Result<RtBatch, SourceError> {
            match &self.behavior {
                Behavior::Ok(b) => Ok(b.clone()),
                Behavior::Empty => Ok(RtBatch::empty()),
                Behavior::Fail => Err("mock failure".into()),
            }
        }
    }

    /// Build an RtBatch whose trip_updates and vehicle_positions each carry `n`
    /// entities (alerts left empty), for exercising non-empty paths.
    pub fn batch_with(n: usize) -> RtBatch {
        let mut m = FeedMessage::default();
        for i in 0..n {
            m.entity.push(FeedEntity {
                id: i.to_string(),
                ..Default::default()
            });
        }
        RtBatch {
            trip_updates: m.clone(),
            vehicle_positions: m,
            alerts: FeedMessage::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::{batch_with, Behavior, MockSource};
    use super::*;

    #[test]
    fn empty_batch_is_empty() {
        assert!(RtBatch::empty().is_empty());
    }

    #[test]
    fn batch_with_entities_is_not_empty() {
        assert!(!batch_with(2).is_empty());
    }

    #[tokio::test]
    async fn mock_source_reports_name_and_batch() {
        let src = MockSource {
            name: "mock",
            behavior: Behavior::Ok(batch_with(1)),
        };
        assert_eq!(src.name(), "mock");
        let batch = src.fetch(&Gtfs::default()).await.unwrap();
        assert_eq!(batch.trip_updates.entity.len(), 1);
    }

    /// #110: the request URL (and anything in it) never reaches the description.
    #[tokio::test]
    async fn describe_error_drops_request_urls_and_keeps_the_cause() {
        let closed_port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let error = reqwest::get(format!(
            "http://user:secret@127.0.0.1:{closed_port}/map?token=abc"
        ))
        .await
        .unwrap_err();
        assert!(error.to_string().contains("token=abc"), "{error}");
        let boxed: SourceError = Box::new(error);
        let described = describe_error(boxed.as_ref());
        assert!(
            described.starts_with("error sending request: "),
            "{described}"
        );
        assert!(
            described.to_ascii_lowercase().contains("connect"),
            "{described}"
        );
        for secret in ["token=abc", "secret", "http://", "/map"] {
            assert!(!described.contains(secret), "{described}");
        }

        let other: SourceError = "fetch https://user:pw@example.com/x?key=1 failed".into();
        assert_eq!(describe_error(other.as_ref()), "fetch <url> failed");

        let long: SourceError = "x".repeat(1000).into();
        assert_eq!(describe_error(long.as_ref()).chars().count(), 303);
    }

    #[tokio::test]
    async fn mock_source_can_fail() {
        let src = MockSource {
            name: "bad",
            behavior: Behavior::Fail,
        };
        assert!(src.fetch(&Gtfs::default()).await.is_err());
    }
}
