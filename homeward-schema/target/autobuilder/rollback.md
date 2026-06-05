# Rollback Plan — homeward-schema

Every commit in this repo is `git revert`-clean.

## How to revert

```bash
# Revert the scaffold commit (iter-1)
git revert 1cfa0ef3169bfa6a6f88499dd4f2c4e30463913c --no-edit

# Or revert to a specific SHA
git reset --hard <sha>
```

## What each commit contains

| SHA | Description | Safe to revert |
|---|---|---|
| 1cfa0ef | iter-1: scaffold homeward-schema with all 7 ACs green | Yes |
