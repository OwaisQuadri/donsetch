"""Head-to-head: DonSeek vs Exa vs Tavily.
Canonical domains defined BEFORE any run (no post-hoc bias).
Fairness control: Exa-Tavily mutual overlap = the parity baseline.
"""
import json, subprocess, threading, time, urllib.request, os, sys
from urllib.parse import urlparse

TAVILY_KEY = os.environ["TAVILY_API_KEY"]  # dev key, see ~/.pi/agent/keys.md
EXA_KEY = os.environ["EXA_API_KEY"]

QUERIES = [
    # (query, canonical domains defined UPFRONT)
    ("rust ownership explained", ["doc.rust-lang.org"]),
    ("javascript fetch api json parse response", ["developer.mozilla.org", "stackoverflow.com"]),
    ("python asyncio gather vs wait", ["docs.python.org", "stackoverflow.com"]),
    ("git rebase onto explained", ["git-scm.com", "stackoverflow.com"]),
    ("attention is all you need transformer paper", ["arxiv.org"]),
    ("retrieval augmented generation paper", ["arxiv.org"]),
    ("nash equilibrium explained", ["en.wikipedia.org", "britannica.com"]),
    ("CRISPR cas9 mechanism explained", ["en.wikipedia.org", "broadinstitute.org"]),
    ("ukraine war latest news", ["reuters.com", "bbc.com", "bbc.co.uk", "apnews.com", "aljazeera.com"]),
    ("nvidia stock price", ["google.com/finance", "finance.yahoo.com", "nasdaq.com", "marketwatch.com", "cnbc.com", "tradingview.com", "macrotrends.net", "investing.com"]),
    ("how to fix a leaking kitchen faucet", ["familyhandyman.com", "homedepot.com", "wikihow.com", "thisoldhouse.com", " lowes.com"]),
    ("best budget mechanical keyboard 2026", ["rtings.com", "pcgamer.com", "tomshardware.com", "wirecutter.com", "nytimes.com"]),
    ("class 12 nepali NEB accounting notes", []),  # niche: no canonical, overlap only
    ("volkswagen jetta 1.9 tdi egr valve cleaning", []),  # long-tail: overlap only
    ("how does japanese pitch accent work", ["en.wikipedia.org", "tofugu.com"]),
    ("mcp protocol json rpc 2.0 specification", ["modelcontextprotocol.io", "spec.modelcontextprotocol.io", "jsonrpc.org"]),
]

def host(u):
    h = urlparse(u).netloc.lower()
    return h[4:] if h.startswith("www.") else h

def canon_hit(results, canonicals):
    for r in results[:3]:
        for c in canonicals:
            if c in host(r.get("url","")) or c in r.get("url","").lower():
                return True
    return False

def tavily(q):
    t0 = time.time()
    req = urllib.request.Request("https://api.tavily.com/search",
        data=json.dumps({"query":q,"max_results":5,"search_depth":"basic"}).encode(),
        headers={"Authorization":f"Bearer {TAVILY_KEY}","Content-Type":"application/json"})
    r = json.loads(urllib.request.urlopen(req, timeout=30).read())
    return r.get("results",[]), time.time()-t0

def exa(q):
    t0 = time.time()
    req = urllib.request.Request("https://api.exa.ai/search",
        data=json.dumps({"query":q,"numResults":5}).encode(),
        headers={"x-api-key":EXA_KEY,"Content-Type":"application/json"})
    r = json.loads(urllib.request.urlopen(req, timeout=30).read())
    return r.get("results",[]), time.time()-t0

# ── DonSeek via MCP daemon ──
env = dict(os.environ)
env["DONSEEK_PROXIES"] = os.environ.get("DONSEEK_PROXIES","")
proc = subprocess.Popen(["/home/dondai/Projects/donsetch/target/release/donsetch","mcp"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1, env=env)
responses = {}
lock = threading.Lock()
def reader():
    for line in proc.stdout:
        try: msg = json.loads(line.strip())
        except: continue
        with lock: responses[msg.get("id")] = msg
threading.Thread(target=reader, daemon=True).start()
def send(m): proc.stdin.write(json.dumps(m)+"\n"); proc.stdin.flush()
def wait(rid, timeout=90):
    t0=time.time()
    while time.time()-t0<timeout:
        with lock:
            if rid in responses: return responses.pop(rid)
        time.sleep(0.05)
    raise TimeoutError(rid)

send({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"bench","version":"0"}}})
wait(0)
rid = 0
def donseek(q):
    global rid
    rid += 1
    send({"jsonrpc":"2.0","id":rid,"method":"tools/call","params":{"name":"search","arguments":{"query":q,"max_results":5}}})
    t0 = time.time()
    r = wait(rid)
    dt = time.time()-t0
    sc = r["result"]["structuredContent"]
    return sc["results"], dt, [e["engine"]+":"+e["status"] for e in sc["engines"]]

def urls(rs): return [r.get("url","") for r in rs[:5]]
def overlap(a, b):
    ua, ub = set(urls(a)), set(urls(b))
    u = len(ua & ub)
    da = {host(x) for x in ua}; db = {host(x) for x in ub}
    return u, len(da & db)

rows = []
for q, canon in QUERIES:
    row = {"query": q, "canon": canon}
    try: tv, tv_t = tavily(q)
    except Exception as e: tv, tv_t = [], -1; print(f"  TAVILY fail: {e}")
    try: ex, ex_t = exa(q)
    except Exception as e: ex, ex_t = [], -1; print(f"  EXA fail: {e}")
    ds, ds_t, engines = donseek(q)
    row.update({"tavily": tv, "exa": ex, "donseek": ds,
                "t_tavily": tv_t, "t_exa": ex_t, "t_donseek": ds_t,
                "engines": engines})
    rows.append(row)
    du_ex, dd_ex = overlap(ds, ex)
    du_tv, dd_tv = overlap(ds, tv)
    bu, bd = overlap(ex, tv)  # baseline: the two paid giants vs each other
    hc = lambda rs: canon_hit(rs, canon) if canon else None
    print(f"\n■ {q}")
    print(f"  DS∩EXA: {du_ex}url/{dd_ex}dom  DS∩TAV: {du_tv}url/{dd_tv}dom  EXA∩TAV(baseline): {bu}url/{bd}dom")
    if canon:
        print(f"  canon@3: donseek={hc(ds)} exa={hc(ex)} tavily={hc(tv)}")
    print(f"  t: DS={ds_t:.1f}s EXA={ex_t:.1f}s TAV={tv_t:.1f}s")

json.dump(rows, open("/home/dondai/Projects/donsetch/bench/results.json","w"), indent=1)
proc.stdin.close(); proc.wait(timeout=10)
print("\nsaved bench/results.json")
