#!/bin/sh
# The gate that proves our metric arithmetic matches agentmemory's own numbers.
#
# It recomputes recall_any@5, recall_any@10 and NDCG@10 from agentmemory's checked-in
# per-question rows using the exact functions in crates/lumberroom/src/eval/mod.rs (reimplemented
# here in Python, since this script has no Rust binary to call), then compares the result against
# the aggregates that ship in the same file. A match here is the evidence that the arithmetic
# side of lumberroom's own retrieval number, computed by the same functions against lumberroom's own
# per-question rows, is not the part that could be wrong.
#
#   ./scripts/eval-metrics-check.sh
#   ./scripts/eval-metrics-check.sh /path/to/hybrid.json /path/to/bm25.json
#
# Reads, by default:
#   ~/.claude/plugins/marketplaces/agentmemory/benchmark/data/longmemeval_results_hybrid.json
#   ~/.claude/plugins/marketplaces/agentmemory/benchmark/data/longmemeval_results_bm25.json
#
# recall_any@20 and MRR are printed as INFO, never as PASS or FAIL. Both files store only the
# first ten retrieved ids per question, so a recomputation cannot see a gold session agentmemory's
# own run found at rank 11 or later; the two numbers being unable to match is expected, not a
# defect in either the harness or the source file.
#
# PASS requires recall_any@5, recall_any@10 and NDCG@10 to match their published aggregate within
# 0.05 percentage points, per file. A file that is absent is SKIP, counted apart from PASS and
# FAIL, and the run exits non-zero if nothing passed: a report of zero passes is not a pass.

set -e
cd "$(dirname "$0")/.."

USAGE="usage: eval-metrics-check.sh [hybrid.json] [bm25.json]

Recomputes recall_any@5, recall_any@10 and NDCG@10 from agentmemory's per-question rows and
checks them against the aggregates in the same file, within 0.05 percentage points. Defaults to
the two files agentmemory ships under ~/.claude/plugins/marketplaces/agentmemory/benchmark/data/."

case "${1:-}" in
  -h|--help) echo "$USAGE"; exit 0 ;;
esac

command -v python3 >/dev/null 2>&1 || { echo "python3 is required" >&2; exit 1; }

DATA_DIR="$HOME/.claude/plugins/marketplaces/agentmemory/benchmark/data"
HYBRID="${1:-$DATA_DIR/longmemeval_results_hybrid.json}"
BM25="${2:-$DATA_DIR/longmemeval_results_bm25.json}"

PASSED=0
SKIPPED=0
FAILED=0

check_file() {
  label="$1"
  path="$2"
  if [ ! -f "$path" ]; then
    echo "SKIP  $label: no file at $path"
    SKIPPED=$((SKIPPED + 1))
    return
  fi
  echo ""
  echo "-- $label: $path --"
  if python3 - "$path" "$label" <<'PY'
import json
import math
import sys


def recall_any(retrieved, gold, k):
    # Mirrors crates/lumberroom/src/eval/mod.rs::recall_any: binary per question, top-k only.
    take = min(len(retrieved), k)
    return 1.0 if any(s in gold for s in retrieved[:take]) else 0.0


def dcg(relevances, k):
    total = 0.0
    for i, rel in enumerate(relevances[:k]):
        if rel:
            total += 1.0 / math.log2(i + 2)
    return total


def ndcg(retrieved, gold, k):
    # Mirrors crates/lumberroom/src/eval/mod.rs::ndcg: binary relevance, ideal ranking puts every
    # gold session first.
    relevances = [s in gold for s in retrieved[:k]]
    ideal = [True] * min(len(gold), k)
    idcg = dcg(ideal, k)
    if idcg <= 0.0:
        return 0.0
    return dcg(relevances, k) / idcg


def mrr(retrieved, gold):
    # Mirrors crates/lumberroom/src/eval/mod.rs::mrr: reciprocal rank of the first gold session.
    for i, s in enumerate(retrieved):
        if s in gold:
            return 1.0 / (i + 1)
    return 0.0


def normalize(value):
    # These files store fractions (0.952), but a percentage (95.2) would silently halve every
    # diff below without this, so the scale is checked rather than assumed.
    if value is None:
        return None
    return value / 100.0 if value > 1.5 else value


path, label = sys.argv[1], sys.argv[2]
with open(path) as f:
    data = json.load(f)

rows = data.get("per_question") or []
if not rows:
    print(f"FAIL  {label}: per_question is empty or missing")
    sys.exit(1)

n = len(rows)
sums = {"r5": 0.0, "r10": 0.0, "r20": 0.0, "ndcg10": 0.0, "mrr": 0.0}
for row in rows:
    retrieved = row.get("retrieved_session_ids") or []
    gold = row.get("gold_session_ids") or []
    sums["r5"] += recall_any(retrieved, gold, 5)
    sums["r10"] += recall_any(retrieved, gold, 10)
    sums["r20"] += recall_any(retrieved, gold, 20)
    sums["ndcg10"] += ndcg(retrieved, gold, 10)
    sums["mrr"] += mrr(retrieved, gold)
computed = {k: v / n for k, v in sums.items()}

published = {
    "r5": normalize(data.get("recall_any_at_5")),
    "r10": normalize(data.get("recall_any_at_10")),
    "r20": normalize(data.get("recall_any_at_20")),
    "ndcg10": normalize(data.get("ndcg_at_10")),
    "mrr": normalize(data.get("mrr")),
}

ok = True


def check(key, name):
    global ok
    c, p = computed[key], published[key]
    if p is None:
        print(f"FAIL  {label} {name}: no published value to compare against")
        ok = False
        return
    diff_pp = abs(c - p) * 100
    verdict = "PASS" if diff_pp <= 0.05 else "FAIL"
    if verdict == "FAIL":
        ok = False
    print(f"{verdict}  {label} {name}: computed {c * 100:.2f}% vs published {p * 100:.2f}% (diff {diff_pp:.3f} pp)")


check("r5", "recall_any@5")
check("r10", "recall_any@10")
check("ndcg10", "NDCG@10")

for key, name in (("r20", "recall_any@20"), ("mrr", "MRR")):
    p = published[key]
    if p is None:
        continue
    c = computed[key]
    print(
        f"INFO  {label} {name}: computed {c * 100:.2f}% vs published {p * 100:.2f}% "
        f"(expected to differ: {label} stores only the first 10 retrieved ids per question, "
        "so a gold session ranked past 10th is invisible to this recomputation)"
    )

sys.exit(0 if ok else 1)
PY
  then
    PASSED=$((PASSED + 1))
  else
    FAILED=$((FAILED + 1))
  fi
}

check_file "hybrid" "$HYBRID"
check_file "bm25" "$BM25"

echo ""
echo "passed=$PASSED skipped=$SKIPPED failed=$FAILED"

if [ "$FAILED" -gt 0 ]; then
  echo "eval-metrics-check FAILED"
  exit 1
fi
if [ "$PASSED" -eq 0 ]; then
  echo "eval-metrics-check found nothing to check: zero passes is not a pass"
  exit 1
fi
echo "eval-metrics-check PASSED"
