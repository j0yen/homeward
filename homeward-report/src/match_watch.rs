//! Background match-watch task for `homeward-reportd`.
//!
//! [`MatchWatcher`] spawns as a tokio background task and polls every
//! `HOMEWARD_MATCH_INTERVAL` seconds (default 600). For each active lost
//! report that has at least one photo, it queries the embed sidecar via
//! [`EmbedClient::query`]. Results with `score >= HOMEWARD_MATCH_THRESHOLD`
//! (default 0.80) are turned into [`MatchAlert`]s and delivered via
//! [`DryRunDeliverer`] + [`DeliveryLedger`].
//!
//! Deduplication is enforced by [`DeliveryLedger::has_delivered`]: the same
//! `(report_id, candidate_id)` pair is never re-delivered.

use std::sync::Arc;

use homeward_embed_client::{EmbedClient, QueryRequest};
use homeward_schema::{LostReport, PetRecord, Species};
use tokio::sync::RwLock;
use tokio::time::{Duration, interval};
use tracing::{debug, info, warn};

use crate::alerts::{AlertConfig, AlertDedup, MatchCandidate, process_candidate};
use crate::delivery::{DeliveryOutcome, Deliverer, DryRunDeliverer};
use crate::delivery_log::DeliveryLedger;
use crate::store::ReportStore;

/// Default poll interval in seconds (override via `HOMEWARD_MATCH_INTERVAL`).
pub const DEFAULT_MATCH_INTERVAL_SECS: u64 = 600;

/// Default match score threshold (override via `HOMEWARD_MATCH_THRESHOLD`).
pub const DEFAULT_MATCH_THRESHOLD: f32 = 0.80;

// ─── Configuration ────────────────────────────────────────────────────────────

/// Runtime configuration for [`MatchWatcher`].
#[derive(Debug, Clone)]
pub struct MatchWatchConfig {
    /// How often to poll (seconds).
    pub interval_secs: u64,
    /// Minimum cosine-similarity score to trigger an alert.
    pub threshold: f32,
    /// Number of nearest neighbours to request per query.
    pub top_k: u32,
}

impl Default for MatchWatchConfig {
    fn default() -> Self {
        Self {
            interval_secs: parse_env_u64("HOMEWARD_MATCH_INTERVAL", DEFAULT_MATCH_INTERVAL_SECS),
            threshold: parse_env_f32("HOMEWARD_MATCH_THRESHOLD", DEFAULT_MATCH_THRESHOLD),
            top_k: 10,
        }
    }
}

fn parse_env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn parse_env_f32(key: &str, default: f32) -> f32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(default)
}

fn species_str(species: Species) -> String {
    match species {
        Species::Dog => "dog".to_owned(),
        Species::Cat => "cat".to_owned(),
    }
}

// ─── MatchWatcher ─────────────────────────────────────────────────────────────

/// Background task that periodically matches active lost reports against the
/// shelter intake pool and delivers [`crate::alerts::MatchAlert`]s for new hits.
pub struct MatchWatcher {
    /// Shared report store (provides `active_reports()`).
    pub store: Arc<RwLock<ReportStore>>,
    /// Shared shelter intake pool (looked up by `canonical_id`).
    pub intake: Arc<RwLock<Vec<PetRecord>>>,
    /// Optional embed sidecar -- when `None` the watcher is a no-op each tick.
    pub embed_client: Option<Arc<EmbedClient>>,
    /// Runtime configuration.
    pub cfg: MatchWatchConfig,
    /// Alert deliverer (swappable for testing).
    pub deliverer: Arc<dyn Deliverer>,
    /// Delivery ledger for persistence + dedup.
    pub ledger: Arc<std::sync::Mutex<DeliveryLedger>>,
}

impl MatchWatcher {
    /// Create a [`MatchWatcher`] with the default [`DryRunDeliverer`] and an
    /// in-memory ledger.
    #[must_use]
    pub fn new(
        store: Arc<RwLock<ReportStore>>,
        intake: Arc<RwLock<Vec<PetRecord>>>,
        embed_client: Option<Arc<EmbedClient>>,
    ) -> Self {
        Self {
            store,
            intake,
            embed_client,
            cfg: MatchWatchConfig::default(),
            deliverer: Arc::new(DryRunDeliverer::new()),
            ledger: Arc::new(std::sync::Mutex::new(DeliveryLedger::in_memory())),
        }
    }

    /// Create a watcher with an explicit config, deliverer, and ledger.
    #[must_use]
    pub fn with_config(
        store: Arc<RwLock<ReportStore>>,
        intake: Arc<RwLock<Vec<PetRecord>>>,
        embed_client: Option<Arc<EmbedClient>>,
        cfg: MatchWatchConfig,
        deliverer: Arc<dyn Deliverer>,
        ledger: Arc<std::sync::Mutex<DeliveryLedger>>,
    ) -> Self {
        Self {
            store,
            intake,
            embed_client,
            cfg,
            deliverer,
            ledger,
        }
    }

    /// Run the watch loop until the task is dropped.
    ///
    /// Ticks at `cfg.interval_secs`. Errors are logged; the loop never panics.
    pub async fn run(self) {
        if self.embed_client.is_none() {
            info!("match_watch: no embed client -- watcher is a no-op");
        }
        let mut ticker = interval(Duration::from_secs(self.cfg.interval_secs));
        ticker.tick().await; // skip first immediate tick
        loop {
            ticker.tick().await;
            self.tick().await;
        }
    }

    /// Execute one watch tick and return the number of alerts delivered.
    pub async fn tick(&self) -> usize {
        let Some(ref client) = self.embed_client else {
            debug!("match_watch tick: no embed client, skipping");
            return 0;
        };

        let reports = self.snapshot_reports().await;
        if reports.is_empty() {
            debug!("match_watch tick: no active reports");
            return 0;
        }

        let intake = self.intake.read().await.clone();
        let alert_cfg = AlertConfig { threshold: self.cfg.threshold };
        let mut dedup = AlertDedup::new();
        let mut total: usize = 0;

        for report in &reports {
            total += self.process_report(report, client, &intake, &alert_cfg, &mut dedup).await;
        }

        info!("match_watch tick: {} alert(s) delivered", total);
        total
    }

    /// Snapshot all active reports from the store (brief read lock).
    async fn snapshot_reports(&self) -> Vec<LostReport> {
        let guard = self.store.read().await;
        guard.active_reports().cloned().collect()
    }

    /// Process one report: query the sidecar and deliver any new alerts.
    ///
    /// Returns the number of alerts delivered for this report.
    async fn process_report(
        &self,
        report: &LostReport,
        client: &EmbedClient,
        intake: &[PetRecord],
        alert_cfg: &AlertConfig,
        dedup: &mut AlertDedup,
    ) -> usize {
        let Some(photo_url) = self.hotlink_photo_url(report) else {
            return 0;
        };

        let req = QueryRequest {
            image_url: Some(photo_url),
            image_b64: None,
            k: self.cfg.top_k,
            species_filter: Some(species_str(report.species)),
        };

        let query_matches = match client.query(req).await {
            Ok(m) => m,
            Err(e) => {
                warn!(report_id = %report.report_id, error = %e,
                    "match_watch: embed query failed, skipping report");
                return 0;
            }
        };

        let candidates: Vec<MatchCandidate> = query_matches
            .into_iter()
            .filter_map(|qm| {
                let record = intake
                    .iter()
                    .find(|r| r.canonical_id.to_string() == qm.canonical_id)?
                    .clone();
                #[allow(clippy::cast_possible_truncation)]
                Some(MatchCandidate {
                    record,
                    score: qm.score as f32,
                    reclaimable_until: None,
                })
            })
            .collect();

        let report_refs: [&LostReport; 1] = [report];
        let now = chrono::Utc::now();
        let mut delivered: usize = 0;

        for candidate in &candidates {
            let alerts = process_candidate(candidate, &report_refs, dedup, alert_cfg, now);
            for alert in alerts {
                if self.deliver_alert(&alert) {
                    delivered += 1;
                }
            }
        }
        delivered
    }

    /// Extract a hotlink photo URL from a report (skips data-URI placeholders).
    fn hotlink_photo_url(&self, report: &LostReport) -> Option<String> {
        let url = report.photos.first().map(|p| p.url.clone())?;
        if url.starts_with("data:") {
            debug!(report_id = %report.report_id,
                "match_watch: skipping report with data-URI photo");
            return None;
        }
        Some(url)
    }

    /// Deliver one alert via the ledger+deliverer pair.
    ///
    /// Returns `true` if the alert was actually delivered (not suppressed/failed).
    fn deliver_alert(&self, alert: &crate::alerts::MatchAlert) -> bool {
        let outcome = {
            let mut ledger = match self.ledger.lock() {
                Ok(g) => g,
                Err(e) => {
                    warn!("match_watch: ledger lock poisoned: {e}");
                    return false;
                }
            };
            self.deliverer.deliver(alert, &mut ledger)
        };
        match &outcome {
            DeliveryOutcome::DryRun { .. } | DeliveryOutcome::Sent { .. } => {
                info!(report_id = %alert.report_id, candidate_id = %alert.candidate_id,
                    outcome = outcome.label(), "match_watch: alert delivered");
                true
            }
            DeliveryOutcome::Suppressed { .. } => {
                debug!(report_id = %alert.report_id, candidate_id = %alert.candidate_id,
                    "match_watch: alert suppressed (already delivered)");
                false
            }
            DeliveryOutcome::Failed { error } => {
                warn!(report_id = %alert.report_id, candidate_id = %alert.candidate_id,
                    error = %error, "match_watch: alert delivery failed");
                false
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::{Duration, Utc};
    use homeward_schema::{
        Availability, BrokeredContactToken, ChipStatus, CoarseLocation, IntakeType, LostReport,
        LostStatus, PhotoRef, PetRecord, ShelterLocation, SourceId, Species, TosClass,
    };
    use tokio::sync::RwLock;
    use ulid::Ulid;

    use crate::alerts::MatchAlert;
    use crate::delivery::{DeliveryOutcome, Deliverer};
    use crate::delivery_log::{DeliveryLedger, DeliveryRecord};
    use crate::store::ReportStore;

    use super::*;

    // ─── Test helpers ─────────────────────────────────────────────────────────

    fn make_report_with_photo(report_id: &str) -> LostReport {
        let now = Utc::now();
        LostReport {
            report_id: report_id.to_owned(),
            species: Species::Dog,
            breed_primary: Some("Labrador".to_owned()),
            breed_secondary: None,
            sex: None,
            age_bucket: None,
            size: None,
            colors: vec![],
            description: None,
            photos: vec![PhotoRef {
                url: "https://example.com/dog.jpg".to_owned(),
                attribution: None,
                is_primary: true,
            }],
            last_seen: CoarseLocation {
                zip_code: Some("78701".to_owned()),
                city: Some("Austin".to_owned()),
                state: Some("TX".to_owned()),
                radius_miles: None,
            },
            contact: BrokeredContactToken::new("tok_0123456789abcdef".to_owned()),
            created: now,
            expires: now + Duration::days(90),
            status: LostStatus::Active,
        }
    }

    fn make_report_no_photo(report_id: &str) -> LostReport {
        let mut r = make_report_with_photo(report_id);
        r.photos = vec![];
        r
    }

    fn make_pet_record(species: Species) -> PetRecord {
        let now = Utc::now();
        PetRecord {
            canonical_id: Ulid::new(),
            source: SourceId::new("test", TosClass::OpenData),
            source_animal_id: None,
            species,
            breed_primary: Some("Mixed".to_owned()),
            breed_secondary: None,
            sex: None,
            age_bucket: None,
            size: None,
            colors: vec![],
            markings_text: None,
            intake_type: IntakeType::Stray,
            availability: Availability::InCustody,
            chip_status: ChipStatus::NotScanned,
            location: Some(ShelterLocation::new(
                None, None, 2,
                "Austin".to_owned(),
                Some("TX".to_owned()),
            )),
            found_location_text: None,
            photos: vec![],
            first_seen: now,
            last_seen: now,
            last_confirmed: None,
            intake_date: Some(now),
            outcome_date: None,
            secondary_provenances: vec![],
        }
    }

    /// A counting deliverer using `AtomicUsize` to avoid `Mutex::lock().expect()`,
    /// which is banned by the workspace `clippy::expect_used` deny.
    #[derive(Debug, Default)]
    struct CountingDeliverer {
        count: AtomicUsize,
    }

    impl Deliverer for CountingDeliverer {
        fn deliver(&self, alert: &MatchAlert, ledger: &mut DeliveryLedger) -> DeliveryOutcome {
            let alert_id = crate::delivery::make_alert_id(alert);
            if ledger.has_delivered(&alert_id) {
                return DeliveryOutcome::Suppressed { alert_id };
            }
            ledger.append(DeliveryRecord {
                alert_id: alert_id.clone(),
                report_id: alert.report_id.clone(),
                deliverer: "CountingDeliverer".to_owned(),
                outcome: "DryRun".to_owned(),
                ts: Utc::now(),
            });
            self.count.fetch_add(1, Ordering::Relaxed);
            DeliveryOutcome::DryRun {
                rendered: format!("delivered {}", alert.candidate_id),
            }
        }
    }

    // ─── Helper: simulate tick logic synchronously without a live sidecar ─────

    fn run_one_tick_sync(
        reports: Vec<LostReport>,
        candidates: Vec<MatchCandidate>,
        threshold: f32,
        ledger: &mut DeliveryLedger,
        deliverer: &dyn Deliverer,
    ) -> usize {
        let alert_cfg = AlertConfig { threshold };
        let mut dedup = AlertDedup::new();
        let now = chrono::Utc::now();
        let mut total = 0usize;

        for report in &reports {
            if report.photos.is_empty() {
                continue;
            }
            let report_refs: [&LostReport; 1] = [report];
            for candidate in &candidates {
                let alerts =
                    process_candidate(candidate, &report_refs, &mut dedup, &alert_cfg, now);
                for alert in alerts {
                    if deliverer.deliver(&alert, ledger).is_delivered() {
                        total += 1;
                    }
                }
            }
        }
        total
    }

    // ─── AC1: alert fires on first match ─────────────────────────────────────

    #[test]
    fn alert_fires_on_first_match() {
        let report = make_report_with_photo("r1");
        let candidate = MatchCandidate {
            record: make_pet_record(Species::Dog),
            score: 0.92,
            reclaimable_until: None,
        };
        let deliverer = CountingDeliverer::default();
        let mut ledger = DeliveryLedger::in_memory();

        let delivered = run_one_tick_sync(
            vec![report], vec![candidate], DEFAULT_MATCH_THRESHOLD, &mut ledger, &deliverer,
        );

        assert_eq!(delivered, 1, "exactly one alert should be delivered");
        assert_eq!(deliverer.count.load(Ordering::Relaxed), 1);
        assert_eq!(ledger.records().len(), 1);
    }

    // ─── AC2: dedup suppresses second delivery ────────────────────────────────

    #[test]
    fn dedup_suppresses_second_delivery() {
        let report = make_report_with_photo("r2");
        let candidate = MatchCandidate {
            record: make_pet_record(Species::Dog),
            score: 0.91,
            reclaimable_until: None,
        };
        let deliverer = CountingDeliverer::default();
        let mut ledger = DeliveryLedger::in_memory();

        let first = run_one_tick_sync(
            vec![report.clone()], vec![candidate.clone()],
            DEFAULT_MATCH_THRESHOLD, &mut ledger, &deliverer,
        );
        assert_eq!(first, 1, "first tick: one delivery");

        let second = run_one_tick_sync(
            vec![report], vec![candidate],
            DEFAULT_MATCH_THRESHOLD, &mut ledger, &deliverer,
        );
        assert_eq!(second, 0, "second tick: deduped, no delivery");
        assert_eq!(
            deliverer.count.load(Ordering::Relaxed), 1,
            "total real deliveries must remain 1"
        );
    }

    // ─── AC3: reports with no photo are skipped ───────────────────────────────

    #[test]
    fn report_without_photo_skipped() {
        let report = make_report_no_photo("r3");
        let candidate = MatchCandidate {
            record: make_pet_record(Species::Dog),
            score: 0.99,
            reclaimable_until: None,
        };
        let deliverer = CountingDeliverer::default();
        let mut ledger = DeliveryLedger::in_memory();

        let delivered = run_one_tick_sync(
            vec![report], vec![candidate], DEFAULT_MATCH_THRESHOLD, &mut ledger, &deliverer,
        );

        assert_eq!(delivered, 0, "no-photo report must produce zero alerts");
        assert_eq!(deliverer.count.load(Ordering::Relaxed), 0);
    }

    // ─── AC4: no embed client => tick returns 0 without panic ─────────────────

    #[tokio::test]
    async fn no_embed_client_tick_completes_without_panic() {
        let store = Arc::new(RwLock::new(ReportStore::new()));
        let intake = Arc::new(RwLock::new(vec![]));
        let watcher = MatchWatcher::new(store, intake, None);

        let count = watcher.tick().await;
        assert_eq!(count, 0, "no-embed tick must return 0 without panic");
    }

    // ─── Below-threshold produces no alert ───────────────────────────────────

    #[test]
    fn below_threshold_produces_no_alert() {
        let report = make_report_with_photo("r5");
        let candidate = MatchCandidate {
            record: make_pet_record(Species::Dog),
            score: 0.50,
            reclaimable_until: None,
        };
        let deliverer = CountingDeliverer::default();
        let mut ledger = DeliveryLedger::in_memory();

        let delivered = run_one_tick_sync(
            vec![report], vec![candidate], DEFAULT_MATCH_THRESHOLD, &mut ledger, &deliverer,
        );

        assert_eq!(delivered, 0, "below-threshold candidate must produce no alert");
    }
}
