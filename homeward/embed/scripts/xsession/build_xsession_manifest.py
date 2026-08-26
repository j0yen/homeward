#!/usr/bin/env python3
"""Build a cross-session holdout manifest from RescueGroups picture metadata.

For each available animal, cluster its pictures into capture sessions
(gap >= SESSION_GAP_DAYS between consecutive `created` timestamps). Keep
animals with >= 2 sessions where the newest session and some earlier
session are 30..730 days apart. Gallery = 1 photo from the NEWEST session
(the shelter's current listing photo); queries = up to 3 photos from the
chosen EARLIER session (the owner's older photos). Stratified: TARGET per
species, <= PER_ORG per org. Metadata only — no image bytes fetched.
"""
import json, os, random, sys, time, urllib.request, datetime as dt
KEY=os.environ["RESCUEGROUPS_API_KEY"]
BASE="https://api.rescuegroups.org/v5/public/animals/search/available"
SESSION_GAP_DAYS=30; MIN_GAP=30; MAX_GAP=730; TARGET=300; PER_ORG=5; MAX_Q=3
PAGE_STRIDE=3  # sample every 3rd page for org diversity
OUT=sys.argv[1]; random.seed(20260825)

def get(url):
    r=urllib.request.Request(url,headers={"Authorization":KEY,"Content-Type":"application/vnd.api+json"})
    for i in range(3):
        try: return json.load(urllib.request.urlopen(r,timeout=40))
        except Exception as e:
            print("retry",i,url[-40:],e,file=sys.stderr); time.sleep(2*(i+1))
    return {}
def ts(s): return dt.datetime.fromisoformat(s.replace("Z","+00:00"))
def org_of(url):
    # orgs relationship is not returned by the search endpoint; the CDN path
    # is https://cdn.rescuegroups.org/<orgId>/pictures/animals/... — use that.
    try: return url.split("cdn.rescuegroups.org/")[1].split("/")[0]
    except Exception: return "?"

rows=[]; stats={"animals":0,"withpics":0,"multi":0,"gap_ok":0}
for sp,label in (("dogs","dog"),("cats","cat")):
    first=get(f"{BASE}/{sp}?limit=250&page=1&include=pictures")
    pages=int(first.get("meta",{}).get("pages",1))
    for page in range(1,pages+1,PAGE_STRIDE):
        d=first if page==1 else get(f"{BASE}/{sp}?limit=250&page={page}&include=pictures")
        pics={}
        for inc in d.get("included",[]):
            if inc["type"]=="pictures":
                at=inc["attributes"]; c=at.get("created"); u=(at.get("large") or {}).get("url") or (at.get("original") or {}).get("url")
                if c and u: pics[inc["id"]]=(ts(c),u,c)
        for a in d.get("data",[]):
            stats["animals"]+=1
            refs=[r["id"] for r in (a.get("relationships",{}).get("pictures",{}).get("data") or [])]
            ps=sorted((pics[r] for r in refs if r in pics),key=lambda x:x[0])
            if not ps: continue
            stats["withpics"]+=1
            sessions=[[ps[0]]]
            for p in ps[1:]:
                if (p[0]-sessions[-1][-1][0]).days>=SESSION_GAP_DAYS: sessions.append([p])
                else: sessions[-1].append(p)
            if len(sessions)<2: continue
            stats["multi"]+=1
            newest=sessions[-1]
            # nearest earlier session with gap in window
            cand=[s for s in sessions[:-1] if MIN_GAP<=(newest[0][0]-s[-1][0]).days<=MAX_GAP]
            if not cand: continue
            stats["gap_ok"]+=1
            q=cand[-1]
            rows.append({"individual_id":f"rg-{a['id']}","species":label,"org":org_of(newest[0][1]),
                "gallery_url":newest[0][1],"gallery_created":newest[0][2],
                "query_urls":[p[1] for p in q[:MAX_Q]],"query_created":[p[2] for p in q[:MAX_Q]],
                "gap_days":(newest[0][0]-q[-1][0]).days,"n_sessions":len(sessions)})
        print(sp,"page",page,"/",pages,"rows so far",len(rows),file=sys.stderr)
        time.sleep(0.5)

with open(OUT+".all","w") as f:
    for r in rows: f.write(json.dumps(r)+"\n")
# stratify
random.shuffle(rows); out=[]; per_org={}; per_sp={"dog":0,"cat":0}
for r in rows:
    k=(r["species"],r["org"])
    if per_sp[r["species"]]>=TARGET or per_org.get(k,0)>=PER_ORG: continue
    per_org[k]=per_org.get(k,0)+1; per_sp[r["species"]]+=1; out.append(r)
with open(OUT,"w") as f:
    for r in out: f.write(json.dumps(r)+"\n")
gaps=sorted(r["gap_days"] for r in out)
print(json.dumps({"stats":stats,"eligible":len(rows),"selected":per_sp,"orgs":len({r['org'] for r in out}),
      "gap_p25_med_p75":[gaps[len(gaps)//4],gaps[len(gaps)//2],gaps[3*len(gaps)//4]] if gaps else None,
      "query_photos":sum(len(r["query_urls"]) for r in out)}))
