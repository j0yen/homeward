#!/usr/bin/env python3
"""Fetch the cross-session holdout into labels.jsonl form.
Gallery line is written FIRST per individual so eval.build_gallery_and_queries
(first-seen image = gallery) uses the newest-session photo as the gallery entry
and the older-session photos as queries."""
import json, sys, time, urllib.request
from pathlib import Path
ROOT=Path(sys.argv[1]); MAN=ROOT/"xsession-manifest.jsonl"; IMG=ROOT/"images"; LABELS=ROOT/"labels.jsonl"; LOG=ROOT/"fetch.log"
UA="homeward-eval/1.0 (+research use)"; last=[0.0]
def get(url,dest,log):
    if dest.exists() and dest.stat().st_size>0: return "skip"
    for i in range(2):
        el=time.monotonic()-last[0]
        if el<0.5: time.sleep(0.5-el)
        last[0]=time.monotonic()
        try:
            data=urllib.request.urlopen(urllib.request.Request(url,headers={"User-Agent":UA}),timeout=15).read()
            dest.parent.mkdir(parents=True,exist_ok=True); dest.write_bytes(data); return "ok"
        except Exception as e:
            print(f"fail {i} {url} {e}",file=log,flush=True)
    return "fail"
rows=[json.loads(l) for l in MAN.open() if l.strip()]
n={"ok":0,"skip":0,"fail":0}; labels=[]
with LOG.open("a") as log:
    for k,r in enumerate(rows):
        iid=r["individual_id"]; sp=r["species"]
        g=IMG/sp/iid/"0_gallery.jpg"
        st=get(r["gallery_url"],g,log); n[st]+=1
        if st=="fail": continue   # no gallery -> drop individual entirely
        labels.append({"individual_id":iid,"image_path":str(g.relative_to(ROOT)),"species":sp,"role":"gallery","created":r["gallery_created"],"gap_days":r["gap_days"]})
        for j,(u,c) in enumerate(zip(r["query_urls"],r["query_created"]),1):
            q=IMG/sp/iid/f"{j}_query.jpg"; st=get(u,q,log); n[st]+=1
            if st!="fail": labels.append({"individual_id":iid,"image_path":str(q.relative_to(ROOT)),"species":sp,"role":"query","created":c})
        if k%50==0: print(f"{k}/{len(rows)} {n}",file=log,flush=True)
LABELS.write_text("".join(json.dumps(l)+"\n" for l in labels))
print(json.dumps({"counts":n,"labels":len(labels),"individuals":len({l['individual_id'] for l in labels})}))
