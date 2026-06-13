//! Enrollment of shelter-intake photos into the embedding gallery.
//!
//! This module wires the ingest delta-event stream to the `homeward-embed`
//! sidecar via [`EmbedClient::enroll`].  Design goals:
//!
//! - **Honest degradation.** If the embed sidecar is absent or unreachable,
//!   enrollment is *skipped with a logged warning* — ingest continues normally.
//!   A counter tracks how many photos went un-enrolled this cycle.
//!
//! - **Idempotent.** Each `(canonical_id, photo_url)` pair is enrolled at most
//!   once per run.  Re-enroll only occurs when the photo set changes.
//!
//! - **Backpressure-aware.** [`EnrollWorker`] consumes from a bounded async
//!   channel so a slow sidecar cannot stall the poll hot path.
//!
//! # Usage
//!
//! ```rust,no_run
//! # use homeward_ingest::enroll::{EnrollWorker, EnrollSink};
//! # use homeward_embed_client::EmbedClientConfig;
//! # #[tokio::main]
//! # async fn main() {
//! let cfg = EmbedClientConfig::from_env();
//! let (sink, worker) = EnrollWorker::new(cfg, 256);
//! tokio::spawn(worker.run());
//! // Pass `sink` to the Orchestrator as the EventSink.
//! # }
//! ```

use std::collections::HashSet;
use std::sync::Arc;

use homeward_embed_client::{EmbedClient, EmbedClientConfig, EmbedClientError, EnrollRequest};
use homeward_schema::Species;
use tokio::sync::mpsc;
use tracing::{info, warn};
use ulid::Ulid;

use crate::events::{EventKind, EventSink, IngestEvent};

// ─── EnrollTask ──────────────────────────────────────────────────────────────

/// Internal task forwarded from the event sink to the worker.
#[derive(Debug)]
struct EnrollTask {
    canonical_id: Ulid,
    photo_url: String,
    species: Option<String>,
}

// ─── EnrollSink ──────────────────────────────────────────────────────────────

/// An [`EventSink`] that enqueues photos for enrollment off the poll hot path.
///
/// Created via [`EnrollWorker::new`].  Cheaply cloneable.
#[derive(Clone, Debug)]
pub struct EnrollSink {
    tx: mpsc::Sender<EnrollTask>,
}

impl EventSink for EnrollSink {
    /// Enqueue enrollment for every new or updated photo.
    ///
    /// Departed events are ignored (removal is a follow-on concern).
    /// If the channel is full, photos are dropped with a warning — never block.
    fn publish(&self, event: IngestEvent) {
        if event.kind == EventKind::Departed {
            return;
        }

        let canonical_id = event.canonical_id;
        let species_hint = match event.record.species {
            Species::Dog => Some("dog".to_owned()),
            Species::Cat => Some("cat".to_owned()),
        };

        for photo in &event.record.photos {
            let task = EnrollTask {
                canonical_id,
                photo_url: photo.url.clone(),
                species: species_hint.clone(),
            };
            if let Err(_e) = self.tx.try_send(task) {
                warn!(
                    canonical_id = %canonical_id,
                    "enroll channel full or closed — dropping photo enrollment"
                );
            }
        }
    }
}

// ─── EnrollWorker ────────────────────────────────────────────────────────────

/// Background worker that drains the enrollment queue.
///
/// Spawn via `tokio::spawn(worker.run())` after calling [`EnrollWorker::new`].
pub struct EnrollWorker {
    rx: mpsc::Receiver<EnrollTask>,
    client: Option<EmbedClient>,
    /// Already-enrolled `(canonical_id, photo_url)` pairs this run (idempotency).
    seen: Arc<std::sync::Mutex<HashSet<(Ulid, String)>>>,
}

impl EnrollWorker {
    /// Construct a worker + sink pair.
    ///
    /// `channel_cap` bounds the backlog; tune to expected burst size.
    ///
    /// If `EmbedClient` construction fails the worker still runs but skips all
    /// enrollments with a warning (honest degradation).
    #[must_use]
    pub fn new(cfg: EmbedClientConfig, channel_cap: usize) -> (EnrollSink, Self) {
        let (tx, rx) = mpsc::channel(channel_cap);
        let client = match EmbedClient::new(cfg) {
            Ok(c) => Some(c),
            Err(e) => {
                warn!(error = %e, "EmbedClient build failed — enrollment will be skipped");
                None
            }
        };
        let sink = EnrollSink { tx };
        let worker = Self {
            rx,
            client,
            seen: Arc::new(std::sync::Mutex::new(HashSet::new())),
        };
        (sink, worker)
    }

    /// Drain the enrollment queue until the channel is closed.
    ///
    /// This is an `async fn` intended to be spawned as a background task.
    pub async fn run(mut self) {
        let Some(ref client) = self.client else {
            warn!("EnrollWorker: no embed client — all enrollment requests will be discarded");
            // drain channel to avoid backpressure buildup
            while self.rx.recv().await.is_some() {}
            return;
        };

        let mut un_enrolled: u64 = 0;

        while let Some(task) = self.rx.recv().await {
            let key = (task.canonical_id, task.photo_url.clone());

            // Idempotency: skip if we've already enrolled this pair this run.
            {
                let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
                if seen.contains(&key) {
                    continue;
                }
                seen.insert(key.clone());
            }

            let req = EnrollRequest::new(
                task.canonical_id,
                task.photo_url.clone(),
                task.species.as_deref(),
            );

            match client.enroll(req).await {
                Ok(resp) => {
                    info!(
                        canonical_id = %task.canonical_id,
                        internal_id = resp.internal_id,
                        detected = resp.detected,
                        "photo enrolled"
                    );
                }
                Err(EmbedClientError::Transport(e)) => {
                    un_enrolled += 1;
                    warn!(
                        canonical_id = %task.canonical_id,
                        url = %task.photo_url,
                        error = %e,
                        un_enrolled,
                        "embedder unavailable — photo un-enrolled"
                    );
                }
                Err(e) => {
                    un_enrolled += 1;
                    warn!(
                        canonical_id = %task.canonical_id,
                        url = %task.photo_url,
                        error = %e,
                        un_enrolled,
                        "enroll call failed — photo un-enrolled"
                    );
                }
            }
        }

        if un_enrolled > 0 {
            warn!(un_enrolled, "embedder unavailable — {} photos un-enrolled this session", un_enrolled);
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use chrono::Utc;
    use homeward_schema::{
        ChipStatus, PetRecord, PhotoRef, Species,
        provenance::{SourceId, TosClass},
        intake::{Availability, IntakeType},
    };
    use tokio::time::Instant;
    use ulid::Ulid;

    use super::*;
    use crate::events::{EventKind, IngestEvent};

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_record_with_photos(urls: &[&str], species: Species) -> PetRecord {
        let mut r = PetRecord {
            canonical_id: Ulid::new(),
            source: SourceId::new("test", TosClass::OpenData),
            source_animal_id: Some("test-001".to_owned()),
            species,
            breed_primary: None,
            breed_secondary: None,
            sex: None,
            age_bucket: None,
            size: None,
            colors: vec![],
            markings_text: None,
            intake_type: IntakeType::Stray,
            availability: Availability::InCustody,
            chip_status: ChipStatus::NotScanned,
            location: None,
            found_location_text: None,
            photos: vec![],
            first_seen: Utc::now(),
            last_seen: Utc::now(),
            last_confirmed: None,
            intake_date: None,
            outcome_date: None,
            secondary_provenances: vec![],
        };
        r.photos = urls
            .iter()
            .map(|u| PhotoRef::new(u.to_string()))
            .collect();
        r
    }

    // ── Collect helper: spin up a tiny HTTP mock that records /enroll bodies ─

    /// Minimal in-process server using `tokio` that accepts POST /enroll and
    /// records the `canonical_id` + `image_url` fields.
    struct MockEmbedServer {
        addr: std::net::SocketAddr,
        received: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl MockEmbedServer {
        async fn start() -> Self {
            use tokio::net::TcpListener;
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let received: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
            let received_clone = Arc::clone(&received);

            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        break;
                    };
                    let received_inner = Arc::clone(&received_clone);
                    tokio::spawn(async move {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};

                        let mut buf = vec![0u8; 4096];
                        let n = stream.read(&mut buf).await.unwrap_or(0);
                        let raw = String::from_utf8_lossy(&buf[..n]);

                        // Crude JSON extraction — good enough for tests.
                        if let Some(body_start) = raw.find("\r\n\r\n") {
                            let body = &raw[body_start + 4..];
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                                let cid = v["canonical_id"].as_str().unwrap_or("").to_owned();
                                let url = v["image_url"].as_str().unwrap_or("").to_owned();
                                received_inner.lock().unwrap().push((cid, url));
                            }
                        }

                        // Return a valid EnrollResponse
                        let resp_body =
                            r#"{"canonical_id":"x","internal_id":1,"detected":true}"#;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            resp_body.len(),
                            resp_body
                        );
                        let _ = stream.write_all(response.as_bytes()).await;
                    });
                }
            });

            Self { addr, received }
        }

        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn snapshot(&self) -> Vec<(String, String)> {
            self.received.lock().unwrap().clone()
        }
    }

    // ── AC2: N photos → N /enroll calls ─────────────────────────────────────

    #[tokio::test]
    async fn ac2_n_photos_produce_n_enroll_calls() {
        let server = MockEmbedServer::start().await;
        let cfg = EmbedClientConfig {
            base_url: server.base_url(),
            timeout: Duration::from_secs(2),
        };
        let (sink, worker) = EnrollWorker::new(cfg, 64);
        let handle = tokio::spawn(worker.run());

        let record = make_record_with_photos(
            &["https://example.com/a.jpg", "https://example.com/b.jpg"],
            Species::Dog,
        );
        let canonical_id = record.canonical_id;
        sink.publish(IngestEvent::new(EventKind::New, record));

        // Give the worker time to drain.
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(sink); // close channel
        let _ = handle.await;

        let calls = server.snapshot();
        assert_eq!(calls.len(), 2, "expected 2 enroll calls, got {:?}", calls);
        for (cid, _url) in &calls {
            assert_eq!(*cid, canonical_id.to_string());
        }
    }

    // ── AC3: no sidecar → ingest completes, warning logged, no panic ─────────

    #[tokio::test]
    async fn ac3_no_sidecar_ingest_completes_without_error() {
        // Use an unoccupied port.
        let cfg = EmbedClientConfig {
            base_url: "http://127.0.0.1:19742".to_owned(),
            timeout: Duration::from_millis(100),
        };
        let (sink, worker) = EnrollWorker::new(cfg, 64);
        let handle = tokio::spawn(worker.run());

        let record = make_record_with_photos(&["https://example.com/c.jpg"], Species::Cat);
        // Publish event — should not panic or block.
        sink.publish(IngestEvent::new(EventKind::New, record));

        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(sink);
        // Worker must terminate cleanly (no panic).
        handle.await.expect("worker should not panic");
    }

    // ── AC4: idempotence — same photo not re-enrolled ────────────────────────

    #[tokio::test]
    async fn ac4_same_photo_not_re_enrolled() {
        let server = MockEmbedServer::start().await;
        let cfg = EmbedClientConfig {
            base_url: server.base_url(),
            timeout: Duration::from_secs(2),
        };
        let (sink, worker) = EnrollWorker::new(cfg, 64);
        let handle = tokio::spawn(worker.run());

        let record = make_record_with_photos(&["https://example.com/dog.jpg"], Species::Dog);
        let canonical_id = record.canonical_id;

        // Publish the same record twice (same canonical_id + URL).
        sink.publish(IngestEvent::new(EventKind::New, record.clone()));
        sink.publish(IngestEvent::new(EventKind::Updated, record));

        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(sink);
        let _ = handle.await;

        let calls = server.snapshot();
        // Only one enroll call — the second is deduplicated.
        assert_eq!(
            calls.len(),
            1,
            "second publish of same (canonical_id, url) should be a no-op, got {:?}",
            calls
        );
        assert_eq!(calls[0].0, canonical_id.to_string());
    }

    // ── AC5: structural — poll path does not await enroll ────────────────────

    #[tokio::test]
    async fn ac5_slow_embedder_does_not_stall_event_publish() {
        // We verify that sink.publish() returns immediately even if the worker
        // is backed up.  Fill the channel, then check publish doesn't block.
        let cfg = EmbedClientConfig {
            base_url: "http://127.0.0.1:19743".to_owned(),
            timeout: Duration::from_millis(50),
        };
        // Cap channel at 2 so it fills fast.
        let (sink, _worker) = EnrollWorker::new(cfg, 2);
        // Don't spawn the worker — channel fills immediately.

        let record = make_record_with_photos(
            &[
                "https://example.com/x.jpg",
                "https://example.com/y.jpg",
                "https://example.com/z.jpg",
            ],
            Species::Dog,
        );

        // publish must return without blocking even when channel is full.
        let start = Instant::now();
        sink.publish(IngestEvent::new(EventKind::New, record));
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(100),
            "publish blocked for {elapsed:?} — must be fire-and-forget"
        );
    }
}
