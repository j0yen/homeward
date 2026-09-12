//! AC5: Given the DB file is absent, When any tool is called, Then a clean
//! typed error returns (no traceback in the client payload) and the
//! process does not exit.

mod common;

use common::{StdioClient, spawn_stdio_server, tool_result_is_error};
use serde_json::json;

#[tokio::test]
async fn missing_db_file_is_a_clean_error_not_a_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Deliberately do NOT create the DB file.
    let missing_db_path = dir.path().join("does-not-exist.db");

    let mut child = spawn_stdio_server(&missing_db_path);
    let mut client = StdioClient::handshake(&mut child).await;

    let result = client
        .call_tool("search_pets", json!({ "species": "dog" }))
        .await;

    assert!(
        result.get("__error__").is_some() || tool_result_is_error(&result),
        "missing DB must produce a typed error: {result:?}"
    );
    let serialized = serde_json::to_string(&result).unwrap_or_default();
    assert!(
        !serialized.contains("panicked at") && !serialized.contains("RUST_BACKTRACE"),
        "error payload must not leak a Rust panic/backtrace: {serialized}"
    );

    // The process must still be alive and answering requests.
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "server process must not have exited after a DB-absent tool call"
    );
    let follow_up = client.request("tools/list", serde_json::Value::Null).await;
    assert!(
        follow_up.get("__error__").is_none(),
        "server must still answer tools/list after a DB-absent call: {follow_up:?}"
    );

    let _ = child.start_kill();
}
