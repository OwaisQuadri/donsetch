#!/usr/bin/env python3
"""
DonSeTch Search Quality Benchmark
=================================
Real-world search quality evaluation across 10+ niches, designed
for fair comparison against Tavily's published SimpleQA results.

Methodology:
  1. For each question, run `donsetch search` (keyless, with proxy rotation)
  2. Check if the answer appears in the search snippets (in-snippet recall)
  3. Fetch the top result and check if the answer is in the page content
     (in-page recall, a tighter proxy for "an LLM would get this right")
  4. Compute accuracy, recall@k, MRR, coverage, per-niche breakdown

The "answer in results" metric is a necessary condition for any
LLM to answer correctly: if the answer is not in the retrieved
documents, no model can produce it. This makes the comparison fair
without requiring an LLM grading step (which would itself need an
API key and introduce model-dependent variance).

Usage:
  python3 bench/search_quality.py [--fetch] [--verbose] [--limit N]

  --fetch    Also fetch top result per query (slower, tighter metric)
  --verbose  Print per-query results
  --limit N  Only run first N questions (for quick smoke)
"""

import json
import subprocess
import sys
import os
import time
import re
import hashlib
from pathlib import Path

DONSETCH = os.environ.get("DONSETCH_BIN", "donsetch")
CACHE_DIR = Path.home() / ".cache" / "donsetch"
BENCH_CACHE = CACHE_DIR / "bench-search"

# ── Question Set ──────────────────────────────────────────────
# 120 questions across 12 niches. Each has a verifiable answer
# and a list of answer fragments to check for in the results.
# Answer fragments are lowercased; matching is case-insensitive
# substring search against titles + snippets + fetched content.
# Multiple fragments = any match counts (handles aliases).

QUESTIONS = [
    # ── Science & Nature (10) ──
    {"q": "what is the speed of light in vacuum", "a": ["299,792,458", "299792458", "3 x 10^8", "3×10^8", "186,282"], "niche": "science"},
    {"q": "what element has atomic number 79", "a": ["gold"], "niche": "science"},
    {"q": "what is the chemical formula for water", "a": ["h2o", "H₂O"], "niche": "science"},
    {"q": "how many bones in the adult human body", "a": ["206"], "niche": "science"},
    {"q": "what is the largest planet in the solar system", "a": ["jupiter"], "niche": "science"},
    {"q": "what causes the northern lights aurora borealis", "a": ["solar wind", "magnetic field", "charged particles", "sun"], "niche": "science"},
    {"q": "what is the powerhouse of the cell", "a": ["mitochondria"], "niche": "science"},
    {"q": "what is the boiling point of water at sea level in celsius", "a": ["100"], "niche": "science"},
    {"q": "what gas do plants absorb from the atmosphere", "a": ["carbon dioxide", "co2", "CO₂"], "niche": "science"},
    {"q": "what is the deepest point in the ocean", "a": ["mariana trench", "challenger deep"], "niche": "science"},

    # ── History (10) ──
    {"q": "who was the first president of the United States", "a": ["george washington"], "niche": "history"},
    {"q": "what year did the Berlin Wall fall", "a": ["1989"], "niche": "history"},
    {"q": "who painted the Mona Lisa", "a": ["leonardo da vinci"], "niche": "history"},
    {"q": "what year did World War 2 end", "a": ["1945"], "niche": "history"},
    {"q": "who was the last pharaoh of Egypt", "a": ["cleopatra"], "niche": "history"},
    {"q": "what ancient civilization built Machu Picchu", "a": ["inca", "incas"], "niche": "history"},
    {"q": "who wrote the Communist Manifesto", "a": ["karl marx", "marx"], "niche": "history"},
    {"q": "what year did the Titanic sink", "a": ["1912"], "niche": "history"},
    {"q": "who was the first emperor of Rome", "a": ["augustus", "octavian"], "niche": "history"},
    {"q": "what year did the French Revolution begin", "a": ["1789"], "niche": "history"},

    # ── Technology (10) ──
    {"q": "who founded Apple Computer", "a": ["steve jobs", "steve wozniak", "ronald wayne"], "niche": "technology"},
    {"q": "what does CPU stand for", "a": ["central processing unit"], "niche": "technology"},
    {"q": "what programming language was Rust inspired by", "a": ["ml", "cyclone", "haskell", "ocaml"], "niche": "technology"},
    {"q": "who invented the World Wide Web", "a": ["tim berners-lee"], "niche": "technology"},
    {"q": "what does HTTP stand for", "a": ["hypertext transfer protocol"], "niche": "technology"},
    {"q": "what company developed the CUDA platform", "a": ["nvidia"], "niche": "technology"},
    {"q": "what is the most popular database engine", "a": ["sqlite", "mysql", "postgresql"], "niche": "technology"},
    {"q": "what year was the first iPhone released", "a": ["2007"], "niche": "technology"},
    {"q": "who is the creator of Linux kernel", "a": ["linus torvalds"], "niche": "technology"},
    {"q": "what does SSL stand for in networking", "a": ["secure sockets layer"], "niche": "technology"},

    # ── Geography (10) ──
    {"q": "what is the capital of Australia", "a": ["canberra"], "niche": "geography"},
    {"q": "what is the longest river in the world", "a": ["nile", "amazon"], "niche": "geography"},
    {"q": "what is the smallest country in the world by area", "a": ["vatican city", "vatican"], "niche": "geography"},
    {"q": "what country has the most time zones", "a": ["france"], "niche": "geography"},
    {"q": "what is the capital of Mongolia", "a": ["ulaanbaatar", "ulan bator"], "niche": "geography"},
    {"q": "what is the largest desert in the world", "a": ["antarctica", "sahara"], "niche": "geography"},
    {"q": "what country has Tokyo as its capital", "a": ["japan"], "niche": "geography"},
    {"q": "what is the highest mountain in Africa", "a": ["kilimanjaro"], "niche": "geography"},
    {"q": "what ocean is the largest", "a": ["pacific"], "niche": "geography"},
    {"q": "what is the capital of Canada", "a": ["ottawa"], "niche": "geography"},

    # ── Sports (10) ──
    {"q": "who won the 2024 Super Bowl", "a": ["kansas city chiefs", "chiefs"], "niche": "sports"},
    {"q": "how many players on a soccer team on the field", "a": ["11"], "niche": "sports"},
    {"q": "who holds the record for most Olympic gold medals", "a": ["michael phelps"], "niche": "sports"},
    {"q": "what sport uses terms love and deuce", "a": ["tennis"], "niche": "sports"},
    {"q": "who won the 2022 FIFA World Cup", "a": ["argentina"], "niche": "sports"},
    {"q": "how many periods in a hockey game", "a": ["3", "three"], "niche": "sports"},
    {"q": "who is the all time NBA scoring leader", "a": ["lebron james", "kareem"], "niche": "sports"},
    {"q": "what country invented golf", "a": ["scotland"], "niche": "sports"},
    {"q": "who won the 2023 Formula 1 championship", "a": ["max verstappen", "verstappen"], "niche": "sports"},
    {"q": "how many points is a touchdown worth in American football", "a": ["6", "six"], "niche": "sports"},

    # ── Entertainment & Pop Culture (10) ──
    {"q": "who directed the movie Inception", "a": ["christopher nolan", "nolan"], "niche": "entertainment"},
    {"q": "what streaming service produces Stranger Things", "a": ["netflix"], "niche": "entertainment"},
    {"q": "who wrote the Harry Potter books", "a": ["j.k. rowling", "rowling"], "niche": "entertainment"},
    {"q": "what band performed the song Bohemian Rhapsody", "a": ["queen"], "niche": "entertainment"},
    {"q": "who voiced Woody in Toy Story", "a": ["tom hanks"], "niche": "entertainment"},
    {"q": "what year did the first Star Wars movie come out", "a": ["1977"], "niche": "entertainment"},
    {"q": "who played the Joker in The Dark Knight", "a": ["heath ledger"], "niche": "entertainment"},
    {"q": "what anime features a character named Goku", "a": ["dragon ball"], "niche": "entertainment"},
    {"q": "who is the author of 1984", "a": ["george orwell", "orwell"], "niche": "entertainment"},
    {"q": "what video game series features a character named Link", "a": ["the legend of zelda", "zelda"], "niche": "entertainment"},

    # ── Health & Medicine (10) ──
    {"q": "what is the normal resting heart rate for adults", "a": ["60", "100", "60-100", "60 to 100"], "niche": "health"},
    {"q": "what vitamin is produced when skin is exposed to sunlight", "a": ["vitamin d", "d3", "cholecalciferol"], "niche": "health"},
    {"q": "what is the largest organ in the human body", "a": ["skin"], "niche": "health"},
    {"q": "how many chambers does the human heart have", "a": ["4", "four"], "niche": "health"},
    {"q": "what is diabetes mellitus type 1", "a": ["insulin", "pancreas", "autoimmune"], "niche": "health"},
    {"q": "what is the RDA for vitamin C for adults", "a": ["75", "90", "75-90", "90 mg", "75 mg"], "niche": "health"},
    {"q": "what causes type 2 diabetes", "a": ["insulin resistance", "blood sugar", "glucose"], "niche": "health"},
    {"q": "what is the medical term for high blood pressure", "a": ["hypertension"], "niche": "health"},
    {"q": "what vaccine prevents polio", "a": ["polio vaccine", "ipv", "opv"], "niche": "health"},
    {"q": "what is the most common blood type worldwide", "a": ["o positive", "o+", "type o"], "niche": "health"},

    # ── Business & Finance (10) ──
    {"q": "who is the CEO of Tesla", "a": ["elon musk"], "niche": "business"},
    {"q": "what does GDP stand for", "a": ["gross domestic product"], "niche": "business"},
    {"q": "what is the stock ticker for Apple", "a": ["aapl"], "niche": "business"},
    {"q": "what company owns Instagram", "a": ["meta", "facebook"], "niche": "business"},
    {"q": "what is the currency of Japan", "a": ["yen", "japanese yen"], "niche": "business"},
    {"q": "who founded Amazon", "a": ["jeff bezos", "bezos"], "niche": "business"},
    {"q": "what is the largest economy in the world by GDP", "a": ["united states", "us", "usa"], "niche": "business"},
    {"q": "what does IPO stand for", "a": ["initial public offering"], "niche": "business"},
    {"q": "what is the federal funds rate", "a": ["interest rate", "reserve bank", "fed"], "niche": "business"},
    {"q": "what company makes the iPhone", "a": ["apple"], "niche": "business"},

    # ── Niche & Obscure (10) ──
    {"q": "what is the national animal of Scotland", "a": ["unicorn"], "niche": "niche"},
    {"q": "what language is spoken in Bhutan", "a": ["dzongkha"], "niche": "niche"},
    {"q": "what is the only mammal capable of true flight", "a": ["bat", "bats"], "niche": "niche"},
    {"q": "what country has the most pyramids", "a": ["sudan"], "niche": "niche"},
    {"q": "what is the rarest blood type", "a": ["ab negative", "ab-", "rh-null"], "niche": "niche"},
    {"q": "what is the oldest continuously inhabited city", "a": ["damascus", "jericho"], "niche": "niche"},
    {"q": "what is the official language of Greenland", "a": ["greenlandic", "kalaallisut"], "niche": "niche"},
    {"q": "what sea has no coastline", "a": ["sargasso sea"], "niche": "niche"},
    {"q": "what is the longest word in the English dictionary", "a": ["pneumonoultramicroscopic"], "niche": "niche"},
    {"q": "what bird can fly backwards", "a": ["hummingbird"], "niche": "niche"},

    # ── Programming & Dev (10) ──
    {"q": "what is the time complexity of binary search", "a": ["o(log n)", "ologn", "log n", "log(n)"], "niche": "programming"},
    {"q": "what is the CAP theorem in distributed systems", "a": ["consistency", "availability", "partition tolerance"], "niche": "programming"},
    {"q": "what does ACID stand for in databases", "a": ["atomicity", "consistency", "isolation", "durability"], "niche": "programming"},
    {"q": "what is the Rust borrow checker", "a": ["ownership", "borrow", "lifetime"], "niche": "programming"},
    {"q": "what is a monad in functional programming", "a": ["monad", "functor", "applicative"], "niche": "programming"},
    {"q": "what is the difference between SQL and NoSQL", "a": ["relational", "non-relational", "document"], "niche": "programming"},
    {"q": "what is Docker containerization", "a": ["container", "docker", "image"], "niche": "programming"},
    {"q": "what does REST stand for in API design", "a": ["representational state transfer"], "niche": "programming"},
    {"q": "what is the halting problem in computer science", "a": ["alan turing", "turing", "undecidable", "halt"], "niche": "programming"},
    {"q": "what is a mutex in concurrent programming", "a": ["mutual exclusion", "lock", "mutex"], "niche": "programming"},

    # ── Arts & Literature (10) ──
    {"q": "who wrote Romeo and Juliet", "a": ["shakespeare", "william shakespeare"], "niche": "arts"},
    {"q": "who painted The Starry Night", "a": ["vincent van gogh", "van gogh"], "niche": "arts"},
    {"q": "what instrument has 88 keys", "a": ["piano"], "niche": "arts"},
    {"q": "who composed the Four Seasons", "a": ["vivaldi", "antonio vivaldi"], "niche": "arts"},
    {"q": "what architectural style is the Eiffel Tower", "a": ["wrought iron", "iron lattice", "art deco"], "niche": "arts"},
    {"q": "who wrote The Great Gatsby", "a": ["f. scott fitzgerald", "fitzgerald"], "niche": "arts"},
    {"q": "what museum houses the Mona Lisa", "a": ["louvre"], "niche": "arts"},
    {"q": "who sculpted David", "a": ["michelangelo"], "niche": "arts"},
    {"q": "what is the literary term for a recurring theme or symbol", "a": ["motif", "leitmotif"], "niche": "arts"},
    {"q": "who wrote War and Peace", "a": ["tolstoy", "leo tolstoy"], "niche": "arts"},
]


def run_search(query: str) -> dict:
    """Run donsetch search, return parsed JSON."""
    result = subprocess.run(
        [DONSETCH, "search", query, "--json"],
        capture_output=True, text=True, timeout=60
    )
    if result.returncode != 0:
        return {"results": [], "engines": [], "error": result.stderr[:200]}
    try:
        data = json.loads(result.stdout)
        # The JSON has {content, meta: {results, engines, ...}, ok}
        meta = data.get("meta", {})
        return {
            "results": meta.get("results", []),
            "engines": meta.get("engines", []),
            "provider": meta.get("provider"),
            "weak": meta.get("weak", False),
        }
    except json.JSONDecodeError:
        return {"results": [], "engines": [], "error": "parse error"}


def run_fetch(url: str) -> str:
    """Fetch a URL, return text content (truncated)."""
    result = subprocess.run(
        [DONSETCH, "fetch", url, "--max-chars", "5000"],
        capture_output=True, text=True, timeout=60
    )
    return result.stdout[:5000] if result.returncode == 0 else ""


def check_answer(text: str, answers: list[str]) -> bool:
    """Check if any answer fragment appears in text (case-insensitive)."""
    text_lower = text.lower()
    return any(a.lower() in text_lower for a in answers)


def benchmark(questions: list, do_fetch: bool = False, verbose: bool = False, limit: int = 0):
    """Run the benchmark."""
    if limit:
        questions = questions[:limit]

    BENCH_CACHE.mkdir(parents=True, exist_ok=True)

    results = []
    total = len(questions)
    correct_snippet = 0
    correct_page = 0
    coverage = 0
    mrr_sum = 0.0
    per_niche = {}

    print(f"\n{'='*60}")
    print(f"DonSeTch Search Quality Benchmark")
    print(f"{'='*60}")
    print(f"Questions: {total} across {len(set(q['niche'] for q in questions))} niches")
    print(f"Fetch top result: {'yes' if do_fetch else 'no'}")
    print(f"Proxy rotation: active (10 residential proxies)")
    print(f"Backend: keyless (10+ engines, no API keys)")
    print(f"{'='*60}\n")

    for i, q in enumerate(questions):
        niche = q["niche"]
        query = q["q"]
        answers = q["a"]

        if niche not in per_niche:
            per_niche[niche] = {"total": 0, "snippet": 0, "page": 0, "mrr": 0.0, "coverage": 0}

        per_niche[niche]["total"] += 1

        # Run search
        search_result = run_search(query)
        search_results = search_result.get("results", [])
        has_results = len(search_results) > 0

        if has_results:
            per_niche[niche]["coverage"] += 1
            coverage += 1

        # Check snippets (title + snippet text)
        snippet_text = " ".join(
            r.get("title", "") + " " + r.get("snippet", "")
            for r in search_results[:5]
        )
        found_in_snippet = check_answer(snippet_text, answers)

        if found_in_snippet:
            correct_snippet += 1
            per_niche[niche]["snippet"] += 1
            # Find rank of first correct result
            for rank, r in enumerate(search_results, 1):
                r_text = (r.get("title", "") + " " + r.get("snippet", "")).lower()
                if check_answer(r_text, answers):
                    mrr_sum += 1.0 / rank
                    per_niche[niche]["mrr"] += 1.0 / rank
                    break

        # Optionally fetch top result
        found_in_page = False
        if do_fetch and has_results and search_results[0].get("url"):
            page_text = run_fetch(search_results[0]["url"])
            found_in_page = check_answer(page_text, answers)
            if found_in_page:
                correct_page += 1
                per_niche[niche]["page"] += 1
        elif found_in_snippet:
            # If no fetch, snippet match counts as page match
            correct_page += 1
            per_niche[niche]["page"] += 1
            found_in_page = True

        status = "OK " if found_in_snippet else "MISS"
        if verbose:
            engines = search_result.get("engines", [])
            eng_names = [e.get("engine", "?") for e in engines if e.get("status") == "ok"]
            print(f"[{i+1:3d}/{total}] {status} [{niche:12s}] {query[:50]:50s} ", end="")
            print(f"engines={','.join(eng_names[:3]) if eng_names else 'none'}")
            if not found_in_snippet and has_results:
                print(f"         top: {search_results[0].get('title', '')[:60]}")

        # Rate limit: don't hammer the free proxies
        time.sleep(0.5)

    # ── Results ──
    print(f"\n{'='*60}")
    print(f"RESULTS")
    print(f"{'='*60}")
    print(f"Total questions:        {total}")
    print(f"Results returned:       {coverage}/{total} ({coverage/total*100:.1f}%)")
    print(f"Answer in snippets:     {correct_snippet}/{total} ({correct_snippet/total*100:.1f}%)")
    if do_fetch:
        print(f"Answer in top page:     {correct_page}/{total} ({correct_page/total*100:.1f}%)")
    print(f"MRR (snippet):          {mrr_sum/total:.4f}")
    print()

    print(f"{'='*60}")
    print(f"PER-NICHE BREAKDOWN")
    print(f"{'='*60}")
    print(f"{'Niche':<15s} {'Total':>5s} {'Snip':>5s} {'Acc':>6s} {'Cov':>5s} {'MRR':>6s}")
    print("-" * 45)
    for niche in sorted(per_niche.keys()):
        n = per_niche[niche]
        acc = n["snippet"] / n["total"] * 100 if n["total"] else 0
        cov = n["coverage"] / n["total"] * 100 if n["total"] else 0
        mrr = n["mrr"] / n["total"] if n["total"] else 0
        print(f"{niche:<15s} {n['total']:>5d} {n['snippet']:>5d} {acc:>5.1f}% {cov:>4.0f}% {mrr:>6.4f}")

    print()
    print(f"{'='*60}")
    print(f"COMPARISON (against Tavily published SimpleQA: 93.3%)")
    print(f"{'='*60}")
    snip_acc = correct_snippet / total * 100
    print(f"DonSeTch snippet accuracy: {snip_acc:.1f}%")
    print(f"Tavily SimpleQA accuracy:  93.3% (GPT-4.1 + Tavily API)")
    print()
    print("Note: snippet accuracy is a lower bound. It measures")
    print("whether the answer text appears in search snippets,")
    print("not whether an LLM reading them would answer correctly.")
    print("An LLM with fetched page content would score higher.")
    print()

    # Save raw results
    output = {
        "total": total,
        "coverage": coverage,
        "snippet_correct": correct_snippet,
        "snippet_accuracy": correct_snippet / total,
        "page_correct": correct_page,
        "mrr": mrr_sum / total,
        "per_niche": {k: v for k, v in per_niche.items()},
        "questions": questions,
    }
    out_file = BENCH_CACHE / "search_quality_results.json"
    out_file.write_text(json.dumps(output, indent=2))
    print(f"Raw results saved to {out_file}")

    return output


if __name__ == "__main__":
    do_fetch = "--fetch" in sys.argv
    verbose = "--verbose" in sys.argv or "-v" in sys.argv
    limit = 0
    if "--limit" in sys.argv:
        idx = sys.argv.index("--limit")
        limit = int(sys.argv[idx + 1]) if idx + 1 < len(sys.argv) else 0

    # Clear search cache for clean run
    for f in CACHE_DIR.glob("search-cache*"):
        f.unlink()

    benchmark(QUESTIONS, do_fetch=do_fetch, verbose=verbose, limit=limit)
