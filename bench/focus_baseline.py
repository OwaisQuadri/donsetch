#!/usr/bin/env python3
"""
Focus parameter baseline + post-redesign test.
Tests focus across diverse page types, measures:
- token output size
- whether focus fell back (no matches)
- whether expected content is present (key phrases)
- whether noise is present (unrelated sections)
- dropped manifest line
"""

import subprocess, json, sys, re, time

BIN = "/home/dondai/Projects/donsetch/target/release/donsetch"

def fetch(url, focus=None, max_chars=16000, section=None):
    """Call donsetch dev extract or use MCP fetch."""
    args = [BIN, "dev", "extract", "--url", url, "--max-chars", str(max_chars)]
    if focus:
        args.extend(["--focus", focus])
    if section:
        args.extend(["--section", section])
    r = subprocess.run(args, capture_output=True, text=True, timeout=60)
    return r.stdout, r.stderr, r.returncode

def count_tokens(text):
    return len(text) // 4

def check_contains(text, phrases):
    """Check which phrases are present."""
    results = {}
    for label, phrase in phrases.items():
        results[label] = phrase.lower() in text.lower()
    return results

def run_test(name, url, focus, expected_present, expected_absent=None, max_chars=16000):
    """Run a single focus test and report results."""
    print(f"\n{'='*80}")
    print(f"TEST: {name}")
    print(f"URL: {url}")
    print(f"FOCUS: {focus}")
    print(f"MAX_CHARS: {max_chars}")
    print(f"{'='*80}")

    stdout, stderr, rc = fetch(url, focus=focus, max_chars=max_chars)
    tokens = count_tokens(stdout)

    # Check for fell-back signal
    fell_back = "no matches" in stdout.lower() and "showing full content" in stdout.lower()
    dropped_manifest = "dropped by focus" in stdout.lower()

    # Check expected content
    present = check_contains(stdout, expected_present) if expected_present else {}

    # Check absent content
    absent_results = {}
    if expected_absent:
        for label, phrase in expected_absent.items():
            absent_results[label] = phrase.lower() not in stdout.lower()

    # Print results
    print(f"\nOutput: {tokens} tokens ({len(stdout)} chars)")
    print(f"Fell back: {fell_back}")
    print(f"Dropped manifest: {dropped_manifest}")
    if dropped_manifest:
        for line in stdout.split('\n'):
            if 'dropped by focus' in line.lower():
                print(f"  Manifest: {line.strip()}")
                break

    print(f"\nExpected content present:")
    for label, found in present.items():
        status = "OK" if found else "MISSING"
        print(f"  [{status}] {label}: '{expected_present[label]}'")

    if absent_results:
        print(f"\nExpected noise absent:")
        for label, absent in absent_results.items():
            status = "OK" if absent else "STILL PRESENT"
            print(f"  [{status}] {label}: '{expected_absent[label]}'")

    # Show first 500 chars of output
    print(f"\nFirst 500 chars of output:")
    print(stdout[:500])
    print("...")

    return {
        "name": name,
        "tokens": tokens,
        "fell_back": fell_back,
        "dropped_manifest": dropped_manifest,
        "present": present,
        "absent": absent_results,
    }

# Also test without focus for comparison
def run_no_focus(name, url, max_chars=16000):
    stdout, _, _ = fetch(url, focus=None, max_chars=max_chars)
    tokens = count_tokens(stdout)
    print(f"\n  [no-focus baseline] {name}: {tokens} tokens ({len(stdout)} chars)")
    return tokens

tests = [
    # 1. Small prose docs page - focus should work well
    {
        "name": "pi MCP servers doc (small prose)",
        "url": "https://pi-agent.dev/docs/mcp",
        "focus": "names and permissions naming convention",
        "expected_present": {
            "naming convention": "<server>_<tool>",
            "permissions": "permission",
        },
        "expected_absent": {
            "oauth": "OAuth",
            "remote servers": "Remote servers",
        },
        "max_chars": 16000,
    },
    # 2. Small prose where focus currently HURTS (plugins doc)
    {
        "name": "pi plugins doc (focus currently hurts)",
        "url": "https://pi-agent.dev/docs/plugins",
        "focus": "plugin structure example format",
        "expected_present": {
            "plugin structure": "create a plugin",
            "export format": "export",
        },
        "expected_absent": {},
        "max_chars": 16000,
    },
    # 3. Code-heavy API docs page
    {
        "name": "Rust std docs (code-heavy)",
        "url": "https://doc.rust-lang.org/std/vec/struct.Vec.html",
        "focus": "push insert elements",
        "expected_present": {
            "push": "push",
            "insert": "insert",
        },
        "expected_absent": {},
        "max_chars": 16000,
    },
    # 4. Wikipedia article (large, structured)
    {
        "name": "Wikipedia: Rust programming language",
        "url": "https://en.wikipedia.org/wiki/Rust_(programming_language)",
        "focus": "memory safety ownership borrow checker",
        "expected_present": {
            "ownership": "ownership",
            "borrow checker": "borrow",
            "memory safety": "memory safety",
        },
        "expected_absent": {
            "history": "first appeared",
        },
        "max_chars": 16000,
    },
    # 5. GitHub README (mixed prose + code)
    {
        "name": "GitHub tokio README",
        "url": "https://raw.githubusercontent.com/tokio-rs/tokio/master/README.md",
        "focus": "async runtime features",
        "expected_present": {
            "async": "async",
            "runtime": "runtime",
        },
        "expected_absent": {},
        "max_chars": 16000,
    },
    # 6. MDN docs page (structured with many sections)
    {
        "name": "MDN Array.prototype.map",
        "url": "https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Array/map",
        "focus": "callback function parameters",
        "expected_present": {
            "callback": "callback",
            "parameters": "element",
        },
        "expected_absent": {},
        "max_chars": 16000,
    },
    # 7. HN thread (forum, nested comments)
    {
        "name": "HN thread (forum)",
        "url": "https://news.ycombinator.com/item?id=42318365",
        "focus": "performance benchmark comparison",
        "expected_present": {},
        "expected_absent": {},
        "max_chars": 16000,
    },
    # 8. Python docs (structured reference)
    {
        "name": "Python asyncio docs",
        "url": "https://docs.python.org/3/library/asyncio.html",
        "focus": "event loop run event loop",
        "expected_present": {
            "event loop": "event loop",
            "run": "run",
        },
        "expected_absent": {},
        "max_chars": 16000,
    },
]

def main():
    phase = sys.argv[1] if len(sys.argv) > 1 else "baseline"
    print(f"\n{'#'*80}")
    print(f"# FOCUS {phase.upper()} TEST")
    print(f"# {time.strftime('%Y-%m-%d %H:%M:%S')}")
    print(f"{'#'*80}")

    results = []

    # First, get no-focus baselines for token comparison
    print(f"\n{'='*80}")
    print("NO-FOCUS BASELINES (full page token counts)")
    print(f"{'='*80}")
    no_focus_baselines = {}
    for t in tests:
        try:
            tokens = run_no_focus(t["name"], t["url"], t["max_chars"])
            no_focus_baselines[t["name"]] = tokens
        except Exception as e:
            print(f"  ERROR: {e}")
            no_focus_baselines[t["name"]] = 0

    # Run focus tests
    for t in tests:
        try:
            result = run_test(
                t["name"], t["url"], t["focus"],
                t.get("expected_present"),
                t.get("expected_absent"),
                t["max_chars"],
            )
            result["no_focus_tokens"] = no_focus_baselines.get(t["name"], 0)
            result["token_savings"] = result["no_focus_tokens"] - result["tokens"]
            results.append(result)
        except Exception as e:
            print(f"  ERROR running test: {e}")

    # Summary
    print(f"\n\n{'#'*80}")
    print(f"# SUMMARY ({phase})")
    print(f"{'#'*80}")
    print(f"\n{'Test':<45} {'No-Focus':>8} {'Focus':>8} {'Saved':>8} {'FB':>4} {'Drop':>5} {'Hit':>5}")
    print("-" * 80)
    for r in results:
        hits = sum(1 for v in r.get("present", {}).values() if v)
        total_expected = len(r.get("present", {}))
        print(f"{r['name']:<45} {r['no_focus_tokens']:>8} {r['tokens']:>8} "
              f"{r['token_savings']:>8} {'Y' if r['fell_back'] else 'N':>4} "
              f"{'Y' if r['dropped_manifest'] else 'N':>5} "
              f"{hits}/{total_expected:>2}")

    # Save results to JSON
    out_file = f"/home/dondai/Projects/donsetch/bench/focus_{phase}_results.json"
    with open(out_file, 'w') as f:
        json.dump(results, f, indent=2, default=str)
    print(f"\nResults saved to {out_file}")

if __name__ == "__main__":
    main()
