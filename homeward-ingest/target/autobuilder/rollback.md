# Rollback plan

HEAD: `af6aa41512225d18463b3e73d4a8c8261027ade0`
Base: `v0.1.1` (`f7563edad85fd66dad5dd3d4c8c7509082297417`)

Reverts are listed newest → oldest. Each `git revert` was
dry-run via `git merge-tree --write-tree` against current HEAD
(merge commits use a real `git revert -m 1` in a scratch
worktree instead), so the caller's working tree was never touched
during verification.

| # | sha | revertable | class | command | subject |
|---|---|---|---|---|---|
| 1 | `af6aa41` | ✓ | substantive | `git revert af6aa41` | agent: refresh intent card for homeward-ingest-backfill |
| 2 | `f990e3a` | ✓ | substantive | `git revert f990e3a` | homeward-ingest: changelog for v0.2.0 backfill release |
| 3 | `d885ce3` | ✓ | substantive | `git revert d885ce3` | homeward-ingest: v0.2.0 — RG population backfill command |
| 4 | `87aded1` | ✓ | mechanical(merge) | `git revert -m 1 87aded1` | Merge branch 'build/homeward-ingest-backfill' |
| 5 | `db0bf81` | ✓ | substantive | `git revert db0bf81` | eval(embed): cross-session holdout — honest lost-vs-found numbers |
| 6 | `0ad0087` | ✓ | substantive | `git revert 0ad0087` | test: fix the two long-standing failures (long_beach catalog, embed small-variant) |
| 7 | `4469294` | ✓ | substantive | `git revert 4469294` | deploy(hub): capture the live constellation hub scaffolding |
| 8 | `a3afc60` | ✓ | substantive | `git revert a3afc60` | feat(wall): integrate homeward-wall crate into the main workspace |
| 9 | `6cccd17` | ✓ | substantive | `git revert 6cccd17` | docs(wall): link the Finding Coconut live match demo |
| 10 | `ebea436` | ✓ | substantive | `git revert ebea436` | feat(embed): dedupe /query matches per animal |
| 11 | `726203a` | ✗ | substantive | `git revert 726203a` | fix(connectors): rescuegroups pagination must use page, not offset |
| 12 | `0a96f55` | ✗ | substantive | `git revert 0a96f55` | fix(connectors): tolerate missing data key on zero-match rescuegroups pages |
| 13 | `8481e96` | ✓ | substantive | `git revert 8481e96` | feat(embed): add DINOv2-large embed variant (1024-d) |
| 14 | `1290a95` | ✓ | substantive | `git revert 1290a95` | fix(connectors): rescuegroups delta cursor must be RFC3339 |

`(M)` marks a commit classified mechanical — `mechanical(pattern)` (matches a known housekeeping shape by both subject line and changed-file set), `mechanical(chain)` (a ≥2-commit same-allowed-path chain where later commits supersede earlier ones), or `mechanical(merge)` (a ≥2-parent commit whose `-m 1` revert is clean) — non-revert-clean but excluded from `blocking_count`/`verdict`.

## Notes

- `af6aa41` — clean revert
- `f990e3a` — clean revert
- `d885ce3` — clean revert
- `87aded1` — clean merge revert (`git revert -m 1`)
- `db0bf81` — clean revert
- `0ad0087` — clean revert
- `4469294` — clean revert
- `a3afc60` — clean revert
- `6cccd17` — clean revert
- `ebea436` — clean revert
- `726203a` — conflicts during revert
- `0a96f55` — conflicts during revert
- `8481e96` — clean revert
- `1290a95` — clean revert
