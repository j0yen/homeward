# Rollback plan

HEAD: `13e3b8be3e44bf3977a4d44df6725473c7dda883`
Base: `v0.2.3` (`50489ad44d4a3615aba18ab28927347fe5fbf232`)

Reverts are listed newest → oldest. Each `git revert` was
dry-run via `git merge-tree --write-tree` against current HEAD
(merge commits use a real `git revert -m 1` in a scratch
worktree instead), so the caller's working tree was never touched
during verification.

No commits in range.
