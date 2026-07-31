#!/usr/bin/env python3
"""DonSeek soak harness: N diverse queries through the MCP
daemon, reporting weak flags, tops, engine health, latency.
Reads DONSEEK_PROXIES from the environment (never inline
credentials). Usage: python3 scripts/soak.py"""
import json, subprocess, threading, time, os, sys

BIN = os.path.join(os.path.dirname(__file__), "..", "target", "release", "donsetch")

proc = subprocess.Popen([os.path.abspath(BIN), "mcp"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    text=True, bufsize=1, env=dict(os.environ))
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

send({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"soak","version":"0"}}})
wait(0)
time.sleep(2.5)  # proxy preflight

queries = [
    "rust ownership explained",
    "attention is all you need paper",
    "nepal earthquake 2015 magnitude",
    "best way to parse json in python",
    "ukraine war latest news",
    "dns over https how it works",
    "react server components tutorial",
    "inflation nepal 2026 rate",
]
lat = []
ok_count = 0
for i, q in enumerate(queries):
    send({"jsonrpc":"2.0","id":i+1,"method":"tools/call","params":{"name":"search","arguments":{"query":q}}})
    t0 = time.time()
    r = wait(i+1)
    dt = time.time()-t0
    sc = r.get("result", {}).get("structuredContent", {})
    lat.append(dt)
    weak = sc.get("weak")
    ok_count += 0 if weak else 1
    eng = ", ".join(f'{e["engine"]}:{e["status"]}' for e in sc.get("engines", []))
    top = sc.get("results", [{}])[0]
    print(f'Q{i+1} [{q[:32]}] {"WEAK" if weak else "OK"} wall={dt:.1f}s cached={sc.get("cached")}')
    print(f'   top: {top.get("title","")[:58]} | {top.get("url","")[:58]}')
    print(f'   {eng}')

proc.stdin.close()
proc.wait(timeout=10)
print(f'\n{ok_count}/{len(queries)} OK | latency avg: {sum(lat)/len(lat):.1f}s max: {max(lat):.1f}s | exit: {proc.returncode}')
sys.exit(0 if ok_count == len(queries) else 1)
