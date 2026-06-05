//! AC1: workspace builds clean and cargo test passes.
//!
//! This test is tautological — if it runs, the build succeeded.
//! The real gate is `cargo build` + `cargo test` exit codes (run by harness).

#[test]
fn ac1_build_and_test_infrastructure() {
    // If this test runs, the library compiled successfully.
    // The cargo test harness verifies all tests pass.
    assert!(true, "build succeeded");
}
