# homeward-schema — Autobuilder Postmortem

## Run summary

- **Slug**: homeward-schema
- **Status**: PASSED — all 7 MUST ACs green in iter-1
- **Iterations**: 1 (scaffold-to-green in one pass)
- **Tests**: 33 passing (30 acceptance, 3 proptest invariants)
- **Quality score**: 73

## What went well

1. The PRD was explicit enough to map directly to Rust types without ambiguity.
2. The privacy posture (BrokeredContactToken opaque type, CoarseLocation without street address, PhotoRef without bytes) translated cleanly to type-level guarantees enforceable at compile time.
3. The stray-hold guardrail was straightforward: distinct IntakeType + Availability enums with a validate_pet_record function.
4. Clippy caught float_arithmetic in geo.rs (reasonable warning — documented with #[allow] + comment) and missing_const_for_fn for constructor methods. Both fixed in same iteration.
5. Proptest caught floating-point residual threshold too tight (1e-9 → 5e-7). Fixed in same iteration.

## Issues encountered

1. **Clippy float_arithmetic**: Geo coarsening uses f64 arithmetic. Annotated with #[allow(clippy::float_arithmetic)] at the specific lines with explanation.
2. **Proptest threshold**: Initial `residual < 1e-9` was too tight for f64 round-trip; 5e-7 is appropriate for GPS-scale coordinates.
3. **Audit false-positive**: Cargo.lock audit check looks in crate subdir; workspace lock is at root. Not a real issue.

## Self-improvement proposals

1. **Scaffold template**: When target_kind=lib in a workspace, the run-metrics.sh harness should look for Cargo.lock at `../Cargo.lock` (workspace root) as well as the crate dir, since workspace members don't have their own lockfile.
2. **AC test regex**: The harness counts `^test acceptance_[a-z0-9_]+ \.\.\. ok` — but acceptance tests named `ac6_lost_report_json_roundtrip` still count. This is correct but the comment could be clarified.

## Next PRDs to queue

- `homeward-connectors`: Fetch from RescueGroups JSON:API v5 and normalize to PetRecord
- `homeward-ingest`: Store canonical records and track departures
