#!/usr/bin/env python3
"""Emit metrics JSON for log_experiment from .gas-results + bytecode size.

Usage: python3 .auto/metrics_json.py
Prints a JSON object with all 27 per-path keys + bytecode_size, suitable
for pasting into log_experiment's `metrics` parameter.
"""
import json
import re

RESULTS = "/home/ralph/code/executor/.gas-results"
BUILD = "/home/ralph/code/executor/.build/__local__.json"

def to_key(label: str) -> str:
    # TestV4V4V4 -> v4_v4_v4
    k = re.sub(r"^Test", "", label)
    k = re.sub(r"([A-Z])", lambda m: "_" + m.group(1).lower(), k)
    return k.lstrip("_")

results = {}
with open(RESULTS) as f:
    for line in f:
        parts = line.split()
        if len(parts) == 3:
            label, gas = parts[1], parts[2]
            results[to_key(label)] = int(gas)

# bytecode size
try:
    import json as _json
    with open(BUILD) as f:
        data = _json.load(f)
    rbc = data.get("contractTypes", {}).get("cmd_executor", {}).get("runtimeBytecode", {}).get("bytecode", "")
    results["bytecode_size"] = (len(rbc) - 2) // 2 if rbc.startswith("0x") else 0
except Exception:
    results["bytecode_size"] = 0

total = sum(v for k, v in results.items() if k != "bytecode_size")
results["__total__"] = total
print(json.dumps({"total": total, "metrics": {k: results[k] for k in sorted(results) if k != "__total__"}}))
