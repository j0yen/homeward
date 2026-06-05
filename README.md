# homeward

A dozen heterogeneous sources describe the same thing — a dog or cat in a shelter

## Overview

A dozen heterogeneous sources describe the same thing — a dog or cat in a shelter
— in a dozen incompatible shapes (RescueGroups JSON:API, municipal Socrata
columns, vendor feeds). Before anything can aggregate, dedup, embed, or match,
there must be **one canonical record** they all normalize into, plus an
owner-side **lost report** type and an honest **provenance** model. This PRD is
that foundation crate: the types, their validation, and their (de)serialization.
It ships no network code — it is the vocabulary the rest of the fleet speaks.


## Acceptance


1. The `homeward` cargo workspace exists with `homeward-schema` as a library
   crate that builds clean (`cargo build`) and passes `cargo test`.
2. `Species` covers `Dog` and `Cat`; constructing a `PetRecord` with an
   unrecognized species string fails with a typed error (no silent default).
3. `PetRecord` keeps `IntakeType` and `Availability` as distinct fields, and a
   validator flags the contradiction "Availability::Adoptable + IntakeType::Stray
   within hold" (the stray-hold guardrail, Phase 1 §3a).
4. `PhotoRef` stores a source URL + optional attribution and **cannot** hold raw
   image bytes (type-level: there is no bytes field) — encoding the hotlink/no-
   bulk-copy copyright posture.
5. `LostReport.contact` is a `BrokeredContactToken` opaque type with no public
   accessor that returns a raw phone/email string; `last_seen` is a coarse
   location type with no street-address field (privacy posture).
6. Every public type round-trips through JSON serde without loss, and
   deserializing a record that is missing any optional field succeeds via serde
   defaults (forward compatibility) — proven by tests.
7. Geo coarsening rounds any provided lat/lon to the configured precision on
   construction; a test asserts a precise coordinate is stored only at coarse
   resolution.

## Install

```sh
cargo install --path .
```

## License

MIT © Joe Yen
