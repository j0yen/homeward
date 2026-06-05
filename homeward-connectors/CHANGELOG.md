# Changelog — homeward-connectors

## v0.1.0 (2026-06-04)

Added `homeward-connectors` crate to the homeward workspace. Implements the
source-connector framework (polite HTTP core, `Connector` trait, `ConnectorRegistry`)
plus two working connectors: `RescueGroupsConnector` (JSON:API v5, `IntakeType::Adoptable`)
and `SocrataConnector` (generic SODA client pre-configured for Austin, Dallas, Sonoma,
and Long Beach municipal shelters). All records normalize into `homeward-schema::PetRecord`.
14 unit tests pass against fixture JSON. Live network calls are not made in tests.
