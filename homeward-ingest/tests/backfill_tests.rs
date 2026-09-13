//! Integration tests for `homeward_ingest::backfill` — PRD-homeward-ingest-backfill.
//!
//! - AC1: seeded DB (100 of 500) + full backfill run -> DB holds all 500, zero duplicate RG ids.
//! - AC2: interrupted-then-resumed run -> resumes from persisted progress, matches AC1's result.
//! - AC3: fixture RG server 429s on every third request -> completes, backoff not failure.
//! - AC4: photo-bearing fixture animals -> embedding entries; id_map.json audited as a list.
//! - AC5: no request ever carries an `offset` param.
//! - AC6: completion report shows per-source fetched/inserted/skipped/failed + db/rg totals.
//! - AC7: `--dry-run`-equivalent (`dry_run_plan`) prints coverage/estimate, DB mtime unchanged.
//! - AC8: empty DB backfill completes with no special-casing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    clippy::missing_const_for_fn,
    clippy::doc_markdown,
    missing_docs
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use homeward_connectors::connectors::rescuegroups::{RescueGroupsConfig, RescueGroupsConnector};
use homeward_ingest::backfill::{self, BackfillConfig};
use homeward_ingest::enroll::EnrollWorker;
use homeward_ingest::store::Store;
use homeward_schema::provenance::{SourceId, TosClass};
use homeward_schema::{ChipStatus, PetRecord, Species, intake::{Availability, IntakeType}};
use ulid::Ulid;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};
use wiremock::matchers::{method, path, query_param};

// ─── Fixture builders ────────────────────────────────────────────────────────

/// One RG `data[]` entry. `with_photo` wires a `pictures` relationship whose
/// `included` counterpart is appended to `included_out`.
fn rg_animal(id: &str, with_photo: bool, included_out: &mut Vec<serde_json::Value>) -> serde_json::Value {
    let relationships = if with_photo {
        let pic_id = format!("{id}-pic");
        included_out.push(serde_json::json!({
            "type": "pictures",
            "id": pic_id,
            "attributes": { "large": { "url": format!("https://cdn.example.com/{id}.jpg") }, "order": 1 },
        }));
        serde_json::json!({
            "colors": null,
            "pictures": { "data": [ { "type": "pictures", "id": pic_id } ] },
        })
    } else {
        serde_json::Value::Null
    };
    serde_json::json!({
        "id": id,
        "type": "animals",
        "attributes": {
            "updatedDate": "2024-01-15T10:00:00Z",
            "createdDate": "2024-01-10T08:00:00Z",
        },
        "relationships": relationships,
    })
}

/// One RG page envelope.
fn rg_page(
    animals: Vec<serde_json::Value>,
    included: Vec<serde_json::Value>,
    page_returned: u64,
    pages: u64,
    count_returned: u64,
    total: u64,
) -> serde_json::Value {
    serde_json::json!({
        "data": animals,
        "meta": {
            "count": total,
            "countReturned": count_returned,
            "pageReturned": page_returned,
            "pages": pages,
            "limit": 250,
            "transactionId": "t",
        },
        "included": included,
    })
}

/// Build the standard 500-animal fixture population used by AC1/AC2/AC5:
/// dogs across 2 pages (250 + 200 = 450), cats on 1 page (50) = 500 total.
struct FiveHundredFixture {
    dogs_page1: serde_json::Value,
    dogs_page2: serde_json::Value,
    cats_page1: serde_json::Value,
    dog_ids: Vec<String>,
    cat_ids: Vec<String>,
}

fn build_500_fixture() -> FiveHundredFixture {
    let mut included = Vec::new();
    let dog_ids: Vec<String> = (1..=450).map(|n| format!("rg-dog-{n:04}")).collect();
    let cat_ids: Vec<String> = (1..=50).map(|n| format!("rg-cat-{n:04}")).collect();

    let dogs_p1: Vec<_> = dog_ids[..250].iter().map(|id| rg_animal(id, false, &mut included)).collect();
    let dogs_p2: Vec<_> = dog_ids[250..].iter().map(|id| rg_animal(id, false, &mut included)).collect();
    let cats_p1: Vec<_> = cat_ids.iter().map(|id| rg_animal(id, false, &mut included)).collect();

    FiveHundredFixture {
        dogs_page1: rg_page(dogs_p1, vec![], 1, 2, 250, 450),
        dogs_page2: rg_page(dogs_p2, vec![], 2, 2, 200, 450),
        cats_page1: rg_page(cats_p1, vec![], 1, 1, 50, 50),
        dog_ids,
        cat_ids,
    }
}

async fn mount_500_fixture(server: &MockServer, fx: &FiveHundredFixture) {
    Mock::given(method("POST"))
        .and(path("/public/animals/search/available/dogs"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fx.dogs_page1))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/public/animals/search/available/dogs"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fx.dogs_page2))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/public/animals/search/available/cats"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fx.cats_page1))
        .mount(server)
        .await;
}

fn connector_for(server: &MockServer) -> RescueGroupsConnector {
    let config = RescueGroupsConfig { api_key: "test-key".to_owned(), base_url: server.uri() };
    RescueGroupsConnector::new(config).expect("connector")
}

/// Seed the store with a pre-existing "rescuegroups" record for the given RG
/// animal id (simulates the 100-of-500 already-present precondition).
fn seed_existing(store: &mut Store, source_animal_id: &str, species: Species) {
    let rec = PetRecord {
        canonical_id: Ulid::new(),
        source: SourceId::new("rescuegroups", TosClass::Api),
        source_animal_id: Some(source_animal_id.to_owned()),
        species,
        breed_primary: None,
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec![],
        markings_text: None,
        intake_type: IntakeType::Adoptable,
        availability: Availability::Adoptable,
        chip_status: ChipStatus::Unknown,
        location: None,
        found_location_text: None,
        photos: vec![],
        first_seen: chrono::Utc::now(),
        last_seen: chrono::Utc::now(),
        last_confirmed: Some(chrono::Utc::now()),
        intake_date: None,
        outcome_date: None,
        secondary_provenances: vec![],
    };
    store.upsert(&rec).expect("seed upsert");
}

// ─── AC1 + AC5 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn ac1_seeded_backfill_reaches_full_population_with_zero_duplicates() {
    let server = MockServer::start().await;
    let fx = build_500_fixture();
    mount_500_fixture(&server, &fx).await;
    let connector = connector_for(&server);

    let mut store = Store::open_in_memory().expect("store");
    // Seed 100 of the 500 (60 dogs, 40 cats) as already present.
    for id in &fx.dog_ids[..60] {
        seed_existing(&mut store, id, Species::Dog);
    }
    for id in &fx.cat_ids[..40] {
        seed_existing(&mut store, id, Species::Cat);
    }
    assert_eq!(store.count().unwrap(), 100);

    let cfg = BackfillConfig::default();
    let report = backfill::run_backfill(&mut store, &connector, &cfg, None)
        .await
        .expect("backfill run");

    assert_eq!(store.count().unwrap(), 500, "DB must hold the full 500-animal population");
    assert_eq!(report.dogs.skipped, 60);
    assert_eq!(report.cats.skipped, 40);
    assert_eq!(report.dogs.inserted, 390);
    assert_eq!(report.cats.inserted, 10);
    assert_eq!(report.dogs.fetched, 450);
    assert_eq!(report.cats.fetched, 50);

    // Zero duplicate RG ids: every animal id resolves to exactly one row.
    for id in fx.dog_ids.iter().chain(fx.cat_ids.iter()) {
        let found = store.find_by_source_animal_id("rescuegroups", id).unwrap();
        assert!(found.is_some(), "expected {id} present after backfill");
    }

    // AC5: no request ever carried an `offset` param — page-based only.
    let requests = server.received_requests().await.expect("recording enabled");
    assert!(!requests.is_empty());
    for req in &requests {
        assert!(
            !req.url.query().unwrap_or_default().contains("offset"),
            "request must never carry offset: {}",
            req.url
        );
        assert!(req.url.query().unwrap_or_default().contains("page="));
    }
}

// ─── AC5 ─────────────────────────────────────────────────────────────────────

/// AC5 as its own test (the assertion also lives inline in AC1's run above,
/// but the acceptance criterion gets a dedicated, independently-named test
/// too): every request the full backfill issues is page-based — never an
/// `offset` param — against a fresh empty store, not piggybacked on AC1's
/// duplicate/count assertions.
#[tokio::test]
async fn ac5_backfill_never_sends_an_offset_param() {
    let server = MockServer::start().await;
    let fx = build_500_fixture();
    mount_500_fixture(&server, &fx).await;
    let connector = connector_for(&server);

    let mut store = Store::open_in_memory().expect("store");
    let cfg = BackfillConfig::default();
    let report = backfill::run_backfill(&mut store, &connector, &cfg, None)
        .await
        .expect("backfill run");

    assert_eq!(report.dogs.fetched, 450);
    assert_eq!(report.cats.fetched, 50);

    let requests = server.received_requests().await.expect("recording enabled");
    assert!(!requests.is_empty(), "backfill must issue at least one request");
    for req in &requests {
        assert!(
            !req.url.query().unwrap_or_default().contains("offset"),
            "request must never carry offset: {}",
            req.url
        );
        assert!(
            req.url.query().unwrap_or_default().contains("page="),
            "request must be page-based: {}",
            req.url
        );
    }
}

// ─── AC2 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ac2_interrupted_backfill_resumes_from_persisted_progress() {
    let server = MockServer::start().await;
    let fx = build_500_fixture();

    Mock::given(method("POST"))
        .and(path("/public/animals/search/available/dogs"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fx.dogs_page1))
        .mount(&server)
        .await;
    // dogs page 2 fails exactly once (simulates the interruption), then
    // succeeds on every subsequent request.
    Mock::given(method("POST"))
        .and(path("/public/animals/search/available/dogs"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/public/animals/search/available/dogs"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fx.dogs_page2))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/public/animals/search/available/cats"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fx.cats_page1))
        .mount(&server)
        .await;

    let connector = connector_for(&server);
    let mut store = Store::open_in_memory().expect("store");
    let cfg = BackfillConfig::default();

    // Round 1: fails partway through dogs (~50%: page 1 of 2 landed, page 2 errors).
    let err = backfill::run_backfill(&mut store, &connector, &cfg, None).await;
    assert!(err.is_err(), "round 1 must fail on the interrupted page");
    assert_eq!(store.count().unwrap(), 250, "page 1's 250 dogs must already be persisted");

    // Round 2: resumes and completes.
    let report = backfill::run_backfill(&mut store, &connector, &cfg, None)
        .await
        .expect("round 2 must complete");
    assert_eq!(store.count().unwrap(), 500, "final DB must match AC1's full result");
    assert_eq!(report.dogs.inserted, 200, "round 2 only inserts the remaining 200 dogs");
    assert_eq!(report.cats.inserted, 50);

    // Resume, not reprocess: dogs page 1 was requested exactly once across
    // both rounds.
    let requests = server.received_requests().await.expect("recording enabled");
    let page1_dog_reqs = requests
        .iter()
        .filter(|r| r.url.path().ends_with("/dogs") && r.url.query().unwrap_or_default().contains("page=1"))
        .count();
    assert_eq!(page1_dog_reqs, 1, "dogs page 1 must not be re-fetched on resume");

    // No duplicates after resume.
    for id in fx.dog_ids.iter().chain(fx.cat_ids.iter()) {
        assert!(store.find_by_source_animal_id("rescuegroups", id).unwrap().is_some());
    }
}

// ─── AC3 ─────────────────────────────────────────────────────────────────────

/// Responds from a fixed `(species, page) -> body` table, shared across
/// every mounted route via a single `Arc<AtomicUsize>` counter so "every
/// third request" counts globally rather than per-route. A 429'd request is
/// retried by the client for the exact same `(species, page)`, so the table
/// lookup — not the counter — decides content once a request is let through.
struct FlakyResponder {
    counter: Arc<AtomicUsize>,
    pages: Arc<std::collections::HashMap<(String, u64), serde_json::Value>>,
}

impl Respond for FlakyResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        if n % 3 == 0 {
            // Instant retry (Retry-After: 0) — exercises the real backoff
            // code path without slowing the test down.
            return ResponseTemplate::new(429).insert_header("Retry-After", "0");
        }
        let species = if request.url.path().ends_with("/dogs") { "dogs" } else { "cats" };
        let page: u64 = request
            .url
            .query_pairs()
            .find(|(k, _)| k == "page")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(1);
        let body = self.pages.get(&(species.to_owned(), page)).expect("known page");
        ResponseTemplate::new(200).set_body_json(body)
    }
}

#[tokio::test]
async fn ac3_rate_limited_backfill_backs_off_and_completes() {
    let server = MockServer::start().await;
    let mut included = Vec::new();

    // Dogs across 2 pages (count_returned=250 on page 1 forces continuation
    // even though the fixture only carries a handful of real records — the
    // loop-termination fields, not the array length, decide pagination) +
    // cats on 1 page = 3 required successful fetches, so request #3
    // (globally counted) is guaranteed to land on a 429 and be retried.
    let dog_ids_p1: Vec<String> = (1..=3).map(|n| format!("rg-dog-p1-{n}")).collect();
    let dog_ids_p2: Vec<String> = (1..=3).map(|n| format!("rg-dog-p2-{n}")).collect();
    let cat_ids: Vec<String> = (1..=3).map(|n| format!("rg-cat-{n}")).collect();

    let dogs_page1 = rg_page(
        dog_ids_p1.iter().map(|id| rg_animal(id, false, &mut included)).collect(),
        vec![],
        1,
        2,
        250, // forces continuation to page 2 regardless of real array length
        6,
    );
    let dogs_page2 = rg_page(
        dog_ids_p2.iter().map(|id| rg_animal(id, false, &mut included)).collect(),
        vec![],
        2,
        2,
        3,
        6,
    );
    let cats_page1 = rg_page(
        cat_ids.iter().map(|id| rg_animal(id, false, &mut included)).collect(),
        vec![],
        1,
        1,
        3,
        3,
    );

    let mut pages = std::collections::HashMap::new();
    pages.insert(("dogs".to_owned(), 1), dogs_page1);
    pages.insert(("dogs".to_owned(), 2), dogs_page2);
    pages.insert(("cats".to_owned(), 1), cats_page1);
    let pages = Arc::new(pages);
    let counter = Arc::new(AtomicUsize::new(0));

    Mock::given(method("POST"))
        .and(path("/public/animals/search/available/dogs"))
        .respond_with(FlakyResponder { counter: Arc::clone(&counter), pages: Arc::clone(&pages) })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/public/animals/search/available/cats"))
        .respond_with(FlakyResponder { counter: Arc::clone(&counter), pages: Arc::clone(&pages) })
        .mount(&server)
        .await;

    let connector = connector_for(&server);
    let mut store = Store::open_in_memory().expect("store");
    let cfg = BackfillConfig::default();

    let report = backfill::run_backfill(&mut store, &connector, &cfg, None)
        .await
        .expect("backfill must complete despite 429s — backoff, not failure");

    assert_eq!(store.count().unwrap(), 9, "all 9 fixture animals must be present");
    assert_eq!(report.dogs.inserted, 6);
    assert_eq!(report.cats.inserted, 3);

    // At least one 429 must actually have fired (proving the backoff path
    // was exercised, not just avoided by luck of request counts): 3
    // required fetches + at least 1 retry means > 3 total requests.
    let total_requests = counter.load(Ordering::SeqCst);
    assert!(total_requests > 3, "expected at least one retried request, got {total_requests}");
}

// ─── AC4 ─────────────────────────────────────────────────────────────────────

/// Minimal in-process embed sidecar stand-in: accepts POST /enroll, records
/// each (canonical_id, species) pair, and persists them to `id_map_path` as
/// a JSON **array** after every enrollment — mirroring the real sidecar's
/// `EmbedIndex._persist_unlocked` behaviour (`id_map.json` is always a list,
/// never a dict; see `backfill::read_id_map_len`'s doc comment).
struct FakeEmbedServer {
    addr: std::net::SocketAddr,
}

impl FakeEmbedServer {
    async fn start(id_map_path: std::path::PathBuf) -> Self {
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let enrolled: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { break };
                let enrolled = Arc::clone(&enrolled);
                let id_map_path = id_map_path.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let raw = String::from_utf8_lossy(&buf[..n]);
                    if let Some(body_start) = raw.find("\r\n\r\n") {
                        let body = &raw[body_start + 4..];
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                            let cid = v["canonical_id"].as_str().unwrap_or("").to_owned();
                            let species = v["species"].as_str().unwrap_or("").to_owned();
                            let list: Vec<_> = {
                                let mut guard = enrolled.lock().unwrap();
                                guard.push((cid, species));
                                guard.iter().cloned().collect()
                            };
                            // Persist as a JSON array, always — the format
                            // this whole AC is guarding.
                            let _ = std::fs::write(&id_map_path, serde_json::to_string(&list).unwrap());
                        }
                    }
                    let resp_body = r#"{"canonical_id":"x","internal_id":1,"detected":true}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp_body.len(),
                        resp_body
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });

        Self { addr }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

#[tokio::test]
async fn ac4_photo_bearing_animals_are_enrolled_and_id_map_audited_as_list() {
    let rg_server = MockServer::start().await;
    let mut included = Vec::new();
    let dog_ids: Vec<String> = (1..=3).map(|n| format!("rg-dog-{n}")).collect();
    let dogs_page = rg_page(
        dog_ids.iter().map(|id| rg_animal(id, true, &mut included)).collect(),
        included,
        1,
        1,
        3,
        3,
    );
    let cats_page = rg_page(vec![], vec![], 1, 0, 0, 0);

    Mock::given(method("POST"))
        .and(path("/public/animals/search/available/dogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&dogs_page))
        .mount(&rg_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/public/animals/search/available/cats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&cats_page))
        .mount(&rg_server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let id_map_path = dir.path().join("id_map.json");
    let embed_server = FakeEmbedServer::start(id_map_path.clone()).await;

    let embed_cfg = homeward_ingest::embed_client::EmbedClientConfig {
        base_url: embed_server.base_url(),
        timeout: Duration::from_secs(2),
    };
    let (sink, worker) = EnrollWorker::new(embed_cfg, 64);
    tokio::spawn(worker.run());

    let connector = connector_for(&rg_server);
    let mut store = Store::open_in_memory().expect("store");
    let cfg = BackfillConfig { id_map_path: Some(id_map_path.clone()) };

    let report = backfill::run_backfill(&mut store, &connector, &cfg, Some(&sink))
        .await
        .expect("backfill run");

    assert_eq!(report.dogs.inserted, 3);
    assert_eq!(report.enroll_candidates, 3, "all 3 photo-bearing inserts must be enroll candidates");

    // Give the async enroll worker a little more headroom in case the CI
    // box is slow, then re-read directly (mirrors what run_backfill's own
    // best-effort audit does internally).
    tokio::time::sleep(Duration::from_millis(300)).await;
    let audit_count = backfill::read_id_map_len(&id_map_path).expect("id_map.json must be readable");
    assert_eq!(audit_count, 3, "audit must read id_map.json as a list, count == enrolled length");
    assert_eq!(report.enrollment_audit, Some(3), "report's own audit must match");

    // Guard against the miscount trap: id_map.json must actually be a JSON
    // array on disk, not an object.
    let raw = std::fs::read_to_string(&id_map_path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(value.is_array(), "id_map.json must be a JSON array, not a dict");
}

// ─── AC6 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ac6_completion_report_has_per_source_counts_and_totals() {
    let server = MockServer::start().await;
    let fx = build_500_fixture();
    mount_500_fixture(&server, &fx).await;
    let connector = connector_for(&server);

    let mut store = Store::open_in_memory().expect("store");
    let cfg = BackfillConfig::default();
    let report = backfill::run_backfill(&mut store, &connector, &cfg, None)
        .await
        .expect("backfill run");

    assert_eq!(report.dogs.fetched, 450);
    assert_eq!(report.dogs.inserted, 450);
    assert_eq!(report.dogs.skipped, 0);
    assert_eq!(report.dogs.failed, 0);
    assert_eq!(report.cats.fetched, 50);
    assert_eq!(report.cats.inserted, 50);
    assert_eq!(report.db_total, 500);
    assert_eq!(report.rg_total_dogs, 450);
    assert_eq!(report.rg_total_cats, 50);
    assert_eq!(report.rg_total(), 500);

    let rendered = report.render();
    assert!(rendered.contains("fetched=450"));
    assert!(rendered.contains("db total: 500"));
    assert!(rendered.contains("rg total: 500"));
}

// ─── AC7 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ac7_dry_run_plan_reports_coverage_and_estimate_without_writing() {
    let server = MockServer::start().await;
    let fx = build_500_fixture();
    mount_500_fixture(&server, &fx).await;
    let connector = connector_for(&server);

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("dry_run_test.db");

    // Seed the DB (and let it close) so a real file with real coverage exists.
    {
        let mut store = Store::open(&db_path).expect("open");
        seed_existing(&mut store, &fx.dog_ids[0], Species::Dog);
        seed_existing(&mut store, &fx.dog_ids[1], Species::Dog);
    }
    let mtime_before = std::fs::metadata(&db_path).unwrap().modified().unwrap();

    let plan = backfill::dry_run_plan(&db_path, &connector).await.expect("dry run plan");

    assert_eq!(plan.current_coverage, 2);
    assert_eq!(plan.dogs_total, 450);
    assert_eq!(plan.dogs_pages, 2);
    assert_eq!(plan.cats_total, 50);
    assert_eq!(plan.cats_pages, 1);

    let mtime_after = std::fs::metadata(&db_path).unwrap().modified().unwrap();
    assert_eq!(mtime_before, mtime_after, "dry-run must not touch the DB file");
}

// ─── AC8 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ac8_empty_db_backfill_completes_without_special_casing() {
    let server = MockServer::start().await;
    let fx = build_500_fixture();
    mount_500_fixture(&server, &fx).await;
    let connector = connector_for(&server);

    let mut store = Store::open_in_memory().expect("store");
    assert_eq!(store.count().unwrap(), 0);

    let cfg = BackfillConfig::default();
    let report = backfill::run_backfill(&mut store, &connector, &cfg, None)
        .await
        .expect("backfill run against an empty DB");

    assert_eq!(store.count().unwrap(), 500);
    assert_eq!(report.dogs.skipped, 0);
    assert_eq!(report.cats.skipped, 0);
}
