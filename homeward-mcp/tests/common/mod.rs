//! Shared fixture-DB and raw-stdio-JSON-RPC test support for `homeward-mcp`'s
//! acceptance tests. These tests spawn the real compiled `homeward-mcp`
//! binary (via `CARGO_BIN_EXE_homeward-mcp`, cargo's own convention for
//! integration tests) over real stdio pipes, so they exercise the exact
//! wire protocol a real MCP client would use.

#![allow(dead_code)]

pub mod mock_embed;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use chrono::{DateTime, Utc};
use homeward_schema::{
    Availability, ChipStatus, IntakeType, PetRecord, PhotoRef, ShelterLocation, Species, SourceId,
    TosClass,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use ulid::Ulid;

const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Build one fixture [`PetRecord`] with sensible defaults, overridable via
/// the passed-in closure.
pub fn make_pet(species: Species, mutate: impl FnOnce(&mut PetRecord)) -> PetRecord {
    let now: DateTime<Utc> = Utc::now();
    let mut record = PetRecord {
        canonical_id: Ulid::new(),
        source: SourceId::new("fixture_source", TosClass::Api),
        source_animal_id: None,
        species,
        breed_primary: Some("Labrador Retriever".to_owned()),
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec!["brown".to_owned()],
        markings_text: None,
        intake_type: IntakeType::Stray,
        availability: Availability::InCustody,
        chip_status: ChipStatus::Unknown,
        location: Some(ShelterLocation::new(
            Some(30.2672),
            Some(-97.7431),
            2,
            "Austin".to_owned(),
            Some("TX".to_owned()),
        )),
        found_location_text: None,
        photos: vec![PhotoRef::new("https://example.org/photo1.jpg".to_owned())],
        first_seen: now,
        last_seen: now,
        last_confirmed: None,
        intake_date: Some(now),
        outcome_date: None,
        secondary_provenances: vec![],
    };
    mutate(&mut record);
    record
}

/// Create a fixture ingest SQLite DB containing `records`, mirroring the
/// `canonical_records` table contract (`canonical_id`, `species`,
/// `record_json`, `availability`, `last_seen`).
pub fn make_fixture_db(dir: &Path, records: &[PetRecord]) -> PathBuf {
    let db_path = dir.join("homeward-ingest.db");
    let conn = Connection::open(&db_path).expect("open fixture db");
    conn.execute(
        "CREATE TABLE canonical_records (
            canonical_id TEXT PRIMARY KEY,
            species TEXT NOT NULL,
            record_json TEXT NOT NULL,
            availability TEXT NOT NULL,
            last_seen TEXT NOT NULL
        )",
        [],
    )
    .expect("create table");

    for record in records {
        let species = match record.species {
            Species::Dog => "dog",
            Species::Cat => "cat",
        };
        let record_json = serde_json::to_string(record).expect("serialize fixture record");
        conn.execute(
            "INSERT INTO canonical_records (canonical_id, species, record_json, availability, last_seen) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                record.canonical_id.to_string(),
                species,
                record_json,
                "in_custody",
                record.last_seen.to_rfc3339(),
            ],
        )
        .expect("insert fixture row");
    }
    db_path
}

/// Spawn the real `homeward-mcp` binary in `serve` (stdio) mode, pointed at
/// `db_path` via `HOMEWARD_INGEST_DB`.
pub fn spawn_stdio_server(db_path: &Path) -> Child {
    let exe = env!("CARGO_BIN_EXE_homeward-mcp");
    Command::new(exe)
        .arg("serve")
        .env("HOMEWARD_INGEST_DB", db_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn homeward-mcp")
}

/// Same as [`spawn_stdio_server`], but also points the embed-sidecar client
/// at `embed_addr` (`HW_EMBED_HOST`/`HW_EMBED_PORT`) -- for `match_photo`
/// tests that need a controllable (mock, or deliberately closed) sidecar.
pub fn spawn_stdio_server_with_embed(db_path: &Path, embed_addr: SocketAddr) -> Child {
    let exe = env!("CARGO_BIN_EXE_homeward-mcp");
    Command::new(exe)
        .arg("serve")
        .env("HOMEWARD_INGEST_DB", db_path)
        .env("HW_EMBED_HOST", embed_addr.ip().to_string())
        .env("HW_EMBED_PORT", embed_addr.port().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn homeward-mcp (with embed env)")
}

/// Spawn the real `homeward-mcp` binary in `serve --http` mode, pointed at
/// `db_path`. Returns the child and the bound address (e.g. `127.0.0.1:PORT`).
pub async fn spawn_http_server(db_path: &Path) -> (Child, String) {
    // Bind to an ephemeral port ourselves, then hand it to the child, to
    // avoid a fixed-port collision between concurrently-running tests.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);

    let exe = env!("CARGO_BIN_EXE_homeward-mcp");
    let child = Command::new(exe)
        .arg("serve")
        .arg("--http")
        .arg(addr.to_string())
        .env("HOMEWARD_INGEST_DB", db_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn homeward-mcp --http");

    // Wait for the healthz endpoint to come up.
    let url = format!("http://{addr}/healthz");
    for _ in 0..50 {
        if reqwest::get(&url).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    (child, addr.to_string())
}

/// A minimal raw JSON-RPC-over-HTTP MCP client (streamable-HTTP, JSON
/// response mode): send `initialize` + `notifications/initialized`, then
/// issue `tools/call`/`resources/list` and return the `result`/`error`
/// field -- same shape as [`StdioClient`], for AC6's parity assertion.
pub struct HttpClient {
    http: reqwest::Client,
    url: String,
    session_id: String,
    next_id: u64,
}

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

impl HttpClient {
    /// Complete the MCP `initialize` handshake against the server at `addr`.
    pub async fn handshake(addr: &str) -> Self {
        let http = reqwest::Client::new();
        let url = format!("http://{addr}/mcp");
        let init_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "homeward-mcp-test", "version": "0.0.0" }
            }
        });
        let response = http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(init_body.to_string())
            .send()
            .await
            .expect("send initialize");
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .expect("initialize response carries mcp-session-id")
            .to_str()
            .expect("session id is ascii")
            .to_owned();
        let _ = response.text().await;

        let client = Self {
            http,
            url,
            session_id,
            next_id: 2,
        };
        client
            .send_notification("notifications/initialized", json!({}))
            .await;
        client
    }

    async fn send_notification(&self, method: &str, params: Value) {
        let _ = self
            .http
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Mcp-Session-Id", &self.session_id)
            .header("Mcp-Protocol-Version", MCP_PROTOCOL_VERSION)
            .body(json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string())
            .send()
            .await;
    }

    /// Send a request and return its `result` (or `{"__error__": ...}`).
    pub async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let response = self
            .http
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Mcp-Session-Id", &self.session_id)
            .header("Mcp-Protocol-Version", MCP_PROTOCOL_VERSION)
            .body(body.to_string())
            .send()
            .await
            .expect("send request");
        let text = response.text().await.expect("response body");
        let value = parse_json_or_sse_body(&text, id)
            .unwrap_or_else(|| panic!("no json-rpc response for id {id} in body: {text}"));
        if let Some(err) = value.get("error") {
            return json!({ "__error__": err.clone() });
        }
        value.get("result").cloned().unwrap_or(Value::Null)
    }

    /// Call a tool by name and return its `result` field.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({ "name": name, "arguments": arguments }))
            .await
    }
}

/// A minimal raw JSON-RPC-over-stdio MCP client: send `initialize` +
/// `notifications/initialized`, then issue `tools/call`/`resources/list`/
/// `resources/read` and return the `result` (or `error`) field.
pub struct StdioClient {
    writer: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
}

impl StdioClient {
    /// Complete the MCP `initialize` handshake against `child`.
    pub async fn handshake(child: &mut Child) -> Self {
        let writer = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let mut client = Self {
            writer,
            reader: BufReader::new(stdout),
            next_id: 1,
        };
        client
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "homeward-mcp-test", "version": "0.0.0" }
                }),
            )
            .await;
        client
            .notify("notifications/initialized", json!({}))
            .await;
        client
    }

    async fn send(&mut self, message: &Value) {
        let mut serialized = serde_json::to_vec(message).expect("serialize request");
        serialized.push(b'\n');
        self.writer
            .write_all(&serialized)
            .await
            .expect("write request");
        self.writer.flush().await.expect("flush request");
    }

    async fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await;
    }

    /// Send a request and return its `result` (or `{"__error__": ...}` on a
    /// JSON-RPC error object, so callers can assert on either).
    pub async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        self.read_response_for_id(id).await
    }

    /// Call a tool by name and return its `result` field.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({ "name": name, "arguments": arguments }))
            .await
    }

    async fn read_response_for_id(&mut self, expected_id: u64) -> Value {
        tokio::time::timeout(READ_TIMEOUT, async {
            loop {
                let mut line = String::new();
                let n = self
                    .reader
                    .read_line(&mut line)
                    .await
                    .expect("read response line");
                assert!(n > 0, "server closed stdout before responding");
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(trimmed).expect("parse json-rpc line");
                let Some(id) = value.get("id").and_then(Value::as_u64) else {
                    continue;
                };
                if id != expected_id {
                    continue;
                }
                if let Some(err) = value.get("error") {
                    return json!({ "__error__": err.clone() });
                }
                return value.get("result").cloned().unwrap_or(Value::Null);
            }
        })
        .await
        .expect("timed out waiting for response")
    }
}

/// Parse a streamable-HTTP response body as either plain JSON or SSE
/// framing (`data: {...}` lines, possibly preceded by an empty keep-alive
/// `data:` event) and return the JSON-RPC message whose `id` matches
/// `expected_id`.
fn parse_json_or_sse_body(body: &str, expected_id: u64) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(body)
        && value.get("id").and_then(Value::as_u64) == Some(expected_id)
    {
        return Some(value);
    }
    for line in body.lines() {
        let Some(payload) = line.strip_prefix("data: ") else {
            continue;
        };
        if payload.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(payload)
            && value.get("id").and_then(Value::as_u64) == Some(expected_id)
        {
            return Some(value);
        }
    }
    None
}

/// Extract the tool-call result's structured content (or parsed first text
/// content block, as a fallback) as a [`Value`].
pub fn tool_result_value(result: &Value) -> Value {
    if let Some(structured) = result.get("structuredContent") {
        return structured.clone();
    }
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// True if a `tools/call` result carries `isError: true`.
pub fn tool_result_is_error(result: &Value) -> bool {
    result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Names of the tools in a `tools/list` result, as an unordered set.
pub fn tool_names(list_result: &Value) -> BTreeMap<String, Value> {
    list_result
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| {
                    let name = t.get("name")?.as_str()?.to_owned();
                    Some((name, t.clone()))
                })
                .collect()
        })
        .unwrap_or_default()
}
