#!/usr/bin/env python3
"""Full legacy embed backfill — every photo-bearing canonical_record not yet
enrolled in the homeward-embed HNSW index, no time window.

Resumable by construction: the worklist is built fresh at *every* startup by
re-reading the sqlite DB (read-only) and the sidecar's on-disk id_map.json —
homeward_embed.index.EmbedIndex.enroll() calls _persist_unlocked() after every
single enrollment, so id_map.json is always current. If this process dies and
is relaunched, it will naturally skip every canonical_id already enrolled
(by itself, by live ingest, or by any other backfill run) with no separate
cursor file needed. Caveat: dedup is per-canonical_id, not per-photo — a
record that dies mid-enrollment (some photos done, some not) will be skipped
whole on the next run. Acceptable per spec (same dedup filter as the prior
outage-window backfill).

Ordered oldest-first (ascending canonical_id / ULID) as requested.
Sequential (concurrency=1) — shares the sidecar with live ingest enrollment
for many hours; never raise concurrency here.

Each photo gets its own timeout + up to 2 retries with backoff before being
logged as FAIL and skipped (does not abort the record or the run).
"""
import json
import sqlite3
import sys
import time
import urllib.request
import urllib.error

DB_PATH = "/home/jsy/.local/share/homeward/homeward-ingest.db"
IDMAP_PATH = "/home/jsy/.local/share/homeward/embed-index/id_map.json"
LOG_PATH = "/home/jsy/.local/share/homeward/backfill-legacy-20260817.log"
SIDECAR_URL = "http://127.0.0.1:8741/enroll"

REQUEST_TIMEOUT_S = 60
MAX_ATTEMPTS = 3  # 1 initial + 2 retries
BACKOFF_S = (2, 5)  # delay before retry 1, retry 2


def now_iso() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def build_worklist():
    """(Re)build the worklist from current DB + id_map state. Returns list of
    (canonical_id, species, [photo_urls]) ordered ascending by canonical_id,
    excluding anything already in the index."""
    con = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True)
    cur = con.cursor()
    cur.execute(
        "SELECT canonical_id, species, record_json FROM canonical_records "
        "WHERE json_array_length(record_json,'$.photos') > 0 "
        "ORDER BY canonical_id ASC"
    )
    rows = cur.fetchall()
    con.close()

    with open(IDMAP_PATH) as f:
        idmap = json.load(f)
    already = set(x[0] for x in idmap)

    worklist = []
    for cid, species, rj in rows:
        if cid in already:
            continue
        rec = json.loads(rj)
        photos = [p["url"] for p in (rec.get("photos") or [])]
        if not photos:
            continue
        worklist.append((cid, species, photos))
    return worklist


def post_enroll(canonical_id: str, image_url: str, species):
    body = json.dumps({
        "canonical_id": canonical_id,
        "image_url": image_url,
        "species": species,
    }).encode("utf-8")
    req = urllib.request.Request(
        SIDECAR_URL, data=body, method="POST",
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT_S) as resp:
        return json.loads(resp.read().decode("utf-8"))


def enroll_with_retry(cid, url, species):
    """Try up to MAX_ATTEMPTS times with backoff. Returns (ok, resp_or_reason)."""
    last_reason = None
    for attempt in range(MAX_ATTEMPTS):
        try:
            resp = post_enroll(cid, url, species)
            return True, resp
        except urllib.error.HTTPError as e:
            body = e.read().decode("utf-8", errors="replace")[:200]
            last_reason = f"http_{e.code} {body}"
        except Exception as e:  # noqa: BLE001
            last_reason = f"{type(e).__name__}: {e}"
        if attempt < MAX_ATTEMPTS - 1:
            time.sleep(BACKOFF_S[attempt])
    return False, last_reason


def main():
    worklist = build_worklist()
    total_records = len(worklist)
    total_photos = sum(len(p) for _, _, p in worklist)

    ok = 0
    fail = 0
    done = 0

    with open(LOG_PATH, "a", buffering=1) as log:
        log.write(f"{now_iso()} === legacy backfill start records={total_records} "
                   f"photos={total_photos} ===\n")
        for cid, species, photos in worklist:
            for url in photos:
                done += 1
                success, result = enroll_with_retry(cid, url, species)
                if success:
                    ok += 1
                    log.write(f"{now_iso()} {cid} OK internal_id={result.get('internal_id')} "
                              f"detected={result.get('detected')} ({done}/{total_photos})\n")
                else:
                    fail += 1
                    log.write(f"{now_iso()} {cid} FAIL {result} ({done}/{total_photos})\n")

        log.write(f"{now_iso()} === backfill done ok={ok} fail={fail} total={total_photos} ===\n")

    print(f"ok={ok} fail={fail} total={total_photos}", file=sys.stderr)


if __name__ == "__main__":
    main()
