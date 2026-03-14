// Rust guideline compliant 2026-02-21
//! Integration tests exercising `PipelineRunner::run()` end-to-end with mock
//! ptrace, covering validation tests 1–7 and 9–13 from `validate.sh`.
//!
//! Test 8 (TLS/keylog capture) is not a ptrace-pipeline test; it is exercised
//! by the keylog/proxy stream integration tests.
//!
//! Unit-level tests for `SharedState` (tests 9/10/11/12) and mock-stream
//! tests (test 13) are in the submodules below.

mod harness;
mod pipeline_tests;
mod state_tests;
mod stream_tests;
