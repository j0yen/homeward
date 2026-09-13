# Rollback plan

HEAD: `cf112a42af678eaba66a90ba08fdb4bdfdb3b906`
Base: `v0.39.0` (`4f2d35e678b0d307cdfa570b3c9c550c461b33ef`)

Reverts are listed newest → oldest. Each `git revert` was
dry-run via `git merge-tree --write-tree` against current HEAD
(merge commits use a real `git revert -m 1` in a scratch
worktree instead), so the caller's working tree was never touched
during verification.

| # | sha | revertable | class | command | subject |
|---|---|---|---|---|---|
| 1 | `cf112a4` | ✓ | substantive | `git revert cf112a4` | homeward-ingest: v0.2.4 — close AC5 backfill-offset test-pairing gap |
| 2 | `4c37b21` | ✓ | substantive | `git revert 4c37b21` | homeward-ingest: regenerate autobuilder receipts (gate run, homeward-ingest-backfill) |
| 3 | `65fe3d2` | ✓ | substantive | `git revert 65fe3d2` | homeward-ingest: add dedicated AC5 test (backfill never sends offset) |
| 4 | `de4e9d4` | ✓ | substantive | `git revert de4e9d4` | homeward-mcp: rename public-reachability test to this PRD's own AC1, add AC2 docs-check |
| 5 | `49b0a08` | ✗ | substantive | `git revert 49b0a08` | homeward-mcp: add AC4 public-reachability test, close hub-deploy's Tailscale-only test gap |

`(M)` marks a commit classified mechanical — `mechanical(pattern)` (matches a known housekeeping shape by both subject line and changed-file set), `mechanical(chain)` (a ≥2-commit same-allowed-path chain where later commits supersede earlier ones), or `mechanical(merge)` (a ≥2-parent commit whose `-m 1` revert is clean) — non-revert-clean but excluded from `blocking_count`/`verdict`.

## Notes

- `cf112a4` — clean revert
- `4c37b21` — clean revert
- `65fe3d2` — clean revert
- `de4e9d4` — clean revert
- `49b0a08` — conflicts during revert
