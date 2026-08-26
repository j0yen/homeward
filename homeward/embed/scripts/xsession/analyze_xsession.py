#!/usr/bin/env python3
"""Slice cross-session eval results by gap length and species."""
import json, sys
from pathlib import Path
X=Path("/home/jsy/homeward-eval/xsession")
gap={}
for l in X.joinpath("labels.jsonl").open():
    o=json.loads(l)
    if o.get("role")=="gallery": gap[o["individual_id"]]=o["gap_days"]
BUCKETS=[(30,60,"30-60d"),(61,120,"61-120d"),(121,365,"121-365d"),(366,730,"1-2y")]
def summarize(rows):
    n=len(rows); 
    if not n: return "n=0"
    r1=sum(1 for r in rows if r["true_rank"]==1)/n; r5=sum(1 for r in rows if r["true_rank"] and r["true_rank"]<=5)/n; r20=sum(1 for r in rows if r["true_rank"])/n
    return f"n={n:4d}  rank1={r1:.3f}  rank5={r5:.3f}  rank20={r20:.3f}"
for v in sys.argv[1:]:
    d=json.load(open(X/f"eval-results-{v}.json")); pq=d["per_query"]
    print(f"\n===== {v}: overall rank1={d['rank1']:.4f} rank5={d['rank5']:.4f} rank20={d['rank20']:.4f} mAP={d['mAP']:.4f} (q={d['n_queries']}, g={d['n_gallery']})")
    for sp in ("dog","cat"): print(f"  {sp:4s}          {summarize([r for r in pq if r['species']==sp])}")
    for lo,hi,name in BUCKETS: print(f"  gap {name:9s} {summarize([r for r in pq if lo<=gap.get(r['individual_id'],-1)<=hi])}")
    # per-individual: any query hit at rank1?
    ind={}
    for r in pq: ind.setdefault(r["individual_id"],[]).append(r["true_rank"]==1)
    print(f"  individuals with >=1 rank-1 hit across their queries: {sum(any(v) for v in ind.values())}/{len(ind)}")
