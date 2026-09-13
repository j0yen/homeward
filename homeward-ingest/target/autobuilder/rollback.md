# Rollback plan

HEAD: `3824da382a6fb7e19b45826e958c85291eabc924`
Base: `v0.2.2` (`3b7f1e4b6d30ba66a51f0ea9a93ee88178f84b5e`)

Reverts are listed newest → oldest. Each `git revert` was
dry-run via `git merge-tree --write-tree` against current HEAD
(merge commits use a real `git revert -m 1` in a scratch
worktree instead), so the caller's working tree was never touched
during verification.

| # | sha | revertable | class | command | subject |
|---|---|---|---|---|---|
| 1 | `3824da3` | ✓ | substantive | `git revert 3824da3` | homeward-ingest: v0.2.3 — symlink Cargo.lock to workspace root, unblocking supply-audit/license-audit/sbom (build-homeward-ingest-autobuilder-onboard) |
| 2 | `f3b20c0` | ✓ | substantive | `git revert f3b20c0` | deploy/hub/README: document the applied homeward-mcp unit |
| 3 | `8da4c66` | ✓ | substantive | `git revert 8da4c66` | tests: add real hub-deploy AC verification scripts (PRD-homeward-mcp-hub-deploy) |
| 4 | `5125ab6` | ✓ | substantive | `git revert 5125ab6` | homeward: regenerate Cargo.lock for homeward-ingest v0.2.2 |

`(M)` marks a commit classified mechanical — `mechanical(pattern)` (matches a known housekeeping shape by both subject line and changed-file set), `mechanical(chain)` (a ≥2-commit same-allowed-path chain where later commits supersede earlier ones), or `mechanical(merge)` (a ≥2-parent commit whose `-m 1` revert is clean) — non-revert-clean but excluded from `blocking_count`/`verdict`.

## Notes

- `3824da3` — clean revert
- `f3b20c0` — clean revert
- `8da4c66` — clean revert
- `5125ab6` — clean revert
