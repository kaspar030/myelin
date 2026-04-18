//! UI tests for `#[myelin::service]`.
//!
//! Each file under `tests/ui/pass_*.rs` must compile; each under
//! `tests/ui/fail_*.rs` must fail with a diagnostic matching the adjacent
//! `.stderr` snapshot. Regenerate snapshots with:
//!
//! ```text
//! TRYBUILD=overwrite cargo test -p myelin-macros --test ui
//! ```

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass_*.rs");
    t.compile_fail("tests/ui/fail_*.rs");
}
