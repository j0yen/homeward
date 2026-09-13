# Rollback plan

HEAD: `e442dc77d0bcfb9de847c1a94bd8b79e3f5760b2`
Base: `v0.1.1` (`f7563edad85fd66dad5dd3d4c8c7509082297417`)

Reverts are listed newest → oldest. Each `git revert` was
dry-run via `git merge-tree --write-tree` against current HEAD
(merge commits use a real `git revert -m 1` in a scratch
worktree instead), so the caller's working tree was never touched
during verification.

| # | sha | revertable | class | command | subject |
|---|---|---|---|---|---|
| 1 | `e442dc7` | ✓ | substantive | `git revert e442dc7` | homeward: commit gate baseline recorded at 39702a6 (repo-wide autobuilder-onboarding debt) |
| 2 | `39702a6` | ✓ | substantive | `git revert 39702a6` | homeward-ingest: gate receipts at 724cb1d (verdict=block, see PRD iter_log) |
| 3 | `724cb1d` | ✗ | substantive | `git revert 724cb1d` | homeward-ingest: gate receipts regenerated at cc195cc (verdict=block, see PRD iter_log) |
| 4 | `cc195cc` | ✗ | substantive | `git revert cc195cc` | homeward-ingest: gate receipts at 86f10e3 (verdict=block, see PRD iter_log) |
| 5 | `86f10e3` | ✓ | substantive | `git revert 86f10e3` | homeward-mcp: also carry intent-card under homeward-mcp/agent/ (gate project-root resolution) |
| 6 | `486c679` | ✓ | substantive | `git revert 486c679` | homeward-mcp: changelog entry names crate version 0.1.0 (ship-postconditions) |
| 7 | `4cf2747` | ✓ | substantive | `git revert 4cf2747` | homeward-mcp: Cargo.lock version bump propagation |
| 8 | `6a01ecd` | ✓ | substantive | `git revert 6a01ecd` | agent: refresh intent card for homeward-mcp-server |
| 9 | `11deccf` | ✗ | substantive | `git revert 11deccf` | homeward-mcp: changelog for v0.37.0 |
| 10 | `39d30aa` | ✓ | substantive | `git revert 39d30aa` | homeward-mcp: v0.37.0 — read-only MCP server (search_pets, get_pet, recent_intakes) |
| 11 | `e930121` | ✗ | substantive | `git revert e930121` | homeward-ingest: gate receipts at af6aa415 (verdict=block, see PRD iter_log) |
| 12 | `af6aa41` | ✓ | substantive | `git revert af6aa41` | agent: refresh intent card for homeward-ingest-backfill |
| 13 | `f990e3a` | ✗ | substantive | `git revert f990e3a` | homeward-ingest: changelog for v0.2.0 backfill release |
| 14 | `d885ce3` | ✓ | substantive | `git revert d885ce3` | homeward-ingest: v0.2.0 — RG population backfill command |
| 15 | `87aded1` | ✓ | mechanical(merge) | `git revert -m 1 87aded1` | Merge branch 'build/homeward-ingest-backfill' |
| 16 | `db0bf81` | ✓ | substantive | `git revert db0bf81` | eval(embed): cross-session holdout — honest lost-vs-found numbers |
| 17 | `0ad0087` | ✓ | substantive | `git revert 0ad0087` | test: fix the two long-standing failures (long_beach catalog, embed small-variant) |
| 18 | `4469294` | ✗ | substantive | `git revert 4469294` | deploy(hub): capture the live constellation hub scaffolding |
| 19 | `a3afc60` | ✗ | substantive | `git revert a3afc60` | feat(wall): integrate homeward-wall crate into the main workspace |
| 20 | `6cccd17` | ✓ | substantive | `git revert 6cccd17` | docs(wall): link the Finding Coconut live match demo |
| 21 | `ebea436` | ✓ | substantive | `git revert ebea436` | feat(embed): dedupe /query matches per animal |
| 22 | `726203a` | ✗ | substantive | `git revert 726203a` | fix(connectors): rescuegroups pagination must use page, not offset |
| 23 | `0a96f55` | ✗ | substantive | `git revert 0a96f55` | fix(connectors): tolerate missing data key on zero-match rescuegroups pages |
| 24 | `8481e96` | ✓ | substantive | `git revert 8481e96` | feat(embed): add DINOv2-large embed variant (1024-d) |
| 25 | `1290a95` | ✗ | substantive | `git revert 1290a95` | fix(connectors): rescuegroups delta cursor must be RFC3339 |

`(M)` marks a commit classified mechanical — `mechanical(pattern)` (matches a known housekeeping shape by both subject line and changed-file set), `mechanical(chain)` (a ≥2-commit same-allowed-path chain where later commits supersede earlier ones), or `mechanical(merge)` (a ≥2-parent commit whose `-m 1` revert is clean) — non-revert-clean but excluded from `blocking_count`/`verdict`.

## Notes

- `e442dc7` — clean revert
- `39702a6` — clean revert
- `724cb1d` — conflicts during revert
- `cc195cc` — conflicts during revert
- `86f10e3` — clean revert
- `486c679` — clean revert
- `4cf2747` — clean revert
- `6a01ecd` — clean revert
- `11deccf` — conflicts during revert
- `39d30aa` — clean revert
- `e930121` — conflicts during revert
- `af6aa41` — clean revert
- `f990e3a` — conflicts during revert
- `d885ce3` — clean revert
- `87aded1` — clean merge revert (`git revert -m 1`)
- `db0bf81` — clean revert
- `0ad0087` — clean revert
- `4469294` — conflicts during revert
- `a3afc60` — conflicts during revert
- `6cccd17` — clean revert
- `ebea436` — clean revert
- `726203a` — conflicts during revert
- `0a96f55` — conflicts during revert
- `8481e96` — clean revert
- `1290a95` — conflicts during revert
