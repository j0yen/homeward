//! AC5: Given a non-image payload and an oversized image, When submitted,
//! Then each yields a typed validation error and the server stays up.
//!
//! Also covers the SSRF-surface half of the same requirement class: a
//! non-http(s) `image_url` scheme must be rejected the same way, before any
//! network call is attempted (see `src/image_input.rs`'s doc comment for
//! why this is a Rust-side allowlist rather than a full SSRF guard).

mod common;

use base64::Engine as _;
use common::mock_embed::MockEmbedServer;
use common::{make_fixture_db, make_pet, spawn_stdio_server_with_embed, tool_result_is_error, StdioClient};
use homeward_schema::Species;
use serde_json::json;

async fn start_server_with_working_embed() -> (tokio::process::Child, tempfile::TempDir, MockEmbedServer) {
    let dir = tempfile::tempdir().expect("tempdir");
    let dog = make_pet(Species::Dog, |_| {});
    let db_path = make_fixture_db(dir.path(), &[dog.clone()]);
    let mock = MockEmbedServer::start(vec![(dog.canonical_id.to_string(), 0.9)]).await;
    let child = spawn_stdio_server_with_embed(&db_path, mock.addr);
    (child, dir, mock)
}

#[tokio::test]
async fn non_image_b64_payload_is_a_typed_error() {
    let (mut child, _dir, _mock) = start_server_with_working_embed().await;
    let mut client = StdioClient::handshake(&mut child).await;

    let junk = base64::engine::general_purpose::STANDARD.encode(b"just some text, not an image");
    let result = client
        .call_tool("match_photo", json!({ "image_b64": junk, "species": "dog" }))
        .await;
    assert!(
        result.get("__error__").is_some() || tool_result_is_error(&result),
        "non-image payload must be a typed error: {result:?}"
    );

    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "server must stay up after a validation error"
    );
    let _ = child.start_kill();
}

#[tokio::test]
async fn oversized_b64_payload_is_a_typed_error() {
    let (mut child, _dir, _mock) = start_server_with_working_embed().await;
    let mut client = StdioClient::handshake(&mut child).await;

    let mut oversized = vec![0xFF, 0xD8, 0xFF];
    oversized.extend(std::iter::repeat_n(0u8, 9 * 1024 * 1024)); // > 8 MiB cap
    let b64 = base64::engine::general_purpose::STANDARD.encode(&oversized);

    let result = client
        .call_tool("match_photo", json!({ "image_b64": b64, "species": "dog" }))
        .await;
    assert!(
        result.get("__error__").is_some() || tool_result_is_error(&result),
        "oversized payload must be a typed error: {result:?}"
    );

    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "server must stay up after a validation error"
    );
    let _ = child.start_kill();
}

#[tokio::test]
async fn non_http_url_scheme_is_a_typed_error() {
    let (mut child, _dir, _mock) = start_server_with_working_embed().await;
    let mut client = StdioClient::handshake(&mut child).await;

    let result = client
        .call_tool(
            "match_photo",
            json!({ "image_url": "file:///etc/passwd", "species": "dog" }),
        )
        .await;
    assert!(
        result.get("__error__").is_some() || tool_result_is_error(&result),
        "a non-http(s) image_url scheme must be a typed error: {result:?}"
    );

    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "server must stay up after a validation error"
    );
    let _ = child.start_kill();
}

#[tokio::test]
async fn missing_both_image_fields_is_a_typed_error() {
    let (mut child, _dir, _mock) = start_server_with_working_embed().await;
    let mut client = StdioClient::handshake(&mut child).await;

    let result = client.call_tool("match_photo", json!({ "species": "dog" })).await;
    assert!(
        result.get("__error__").is_some() || tool_result_is_error(&result),
        "missing both image_url and image_b64 must be a typed error: {result:?}"
    );

    let _ = child.start_kill();
}
