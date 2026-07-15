#!/usr/bin/env python3
"""Verify every whitelisted ERC-20 is tax-free via the balance-delta technique.

For each token: scan recent Transfer events, isolate a sample whose recipient
received exactly ONE inbound transfer of that token in its block (eliminates
multi-transfer contamination), then read recipient balanceOf at block-1 vs
block and confirm delta == emitted amount.

A fee-on-transfer / tax-on-receipt token shows delta < amount on EVERY sample
(the recipient is always shorted). A tax-free token matches on the first clean
sample. WETH's deposit()/withdrawal() also touch balanceOf, so contaminated
samples are rejected and the scan continues.
"""
from __future__ import annotations

import json
import subprocess
from collections import defaultdict
from dataclasses import dataclass

RP = "http://host.containers.internal:8545"
TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
ZERO = "0x0000000000000000000000000000000000000000"
# Burn/sink addresses: transfers TO these don't change balanceOf predictably
# (0x0 = mint/burn endpoint, 0x…dead = the canonical burn sink, 0x…1 = the
# ecrecover precompile some tokens burn to). Excluding these as recipients
# prevents false-positive "tax" verdicts from burn-to-dead transfers that
# otherwise look like user transfers (to != 0x0) but settle to a sink whose
# balanceOf is irrelevant or refilled out-of-band.
BURN_SINKS = {
    ZERO,
    "0x000000000000000000000000000000000000dead",
    "0x0000000000000000000000000000000000000001",
}
# Env-configurable so candidate-batch runs (high-volume tokens like PEPE/SHIB)
# can use a smaller scan window + sample cap without editing the source.
import os
SCAN_BLOCKS = int(os.environ.get("SCAN_BLOCKS", "2000"))
MAX_SAMPLES = int(os.environ.get("MAX_SAMPLES", "60"))

import sys

# CLI usage:
#   python3 scripts/verify_tokens.py                 # verify the whitelist below
#   python3 scripts/verify_tokens.py WLD DAI          # verify just those symbols
#   python3 scripts/verify_tokens.py 0xADDR:Symb ...  # also verify those addr:sym pairs
#       (merge-in candidates, e.g. discovered via a DB high-degree scan)
_argv = sys.argv[1:]
_SCAN_SYMBOLS: set[str] | None = None
_EXTRA: dict[str, str] = {}  # address -> symbol
if _argv:
    if all(":" in a for a in _argv):
        # all addr:sym pairs — no symbol filter; merge + verify everything
        for a in _argv:
            addr, _, sym = a.partition(":")
            _EXTRA[addr] = sym or addr[:10]
    else:
        # plain symbols — filter the whitelist to these
        _SCAN_SYMBOLS = set(_argv)
TOKENS = {
    "0x163f8C2467924be0ae7B5347228CABF260318753": "WLD",
    "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8": "PyUSD",
    "0xB8c77482e45F1F44dE1745F52C74426C631bDD52": "BNB",
    "0x7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0": "wstETH",
    "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2": "WETH",
    "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48": "USDC",
    "0xdAC17F958D2ee523a2206206994597C13D831ec7": "USDT",
    "0x6B175474E89094C44Da98b954EedeAC495271d0F": "DAI",
    "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599": "WBTC",
    "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984": "UNI",
    "0x514910771AF9Ca656af840dff83E8264EcF986CA": "LINK",
    "0x6B3595068778DD592e39A122f4f5a5cF09C90fE2": "SUSHI",
    "0xD533a949740bb3306d119CC777fa900bA034cd52": "CRV",
    "0xc00e94Cb662C3520282E6f5717214004A7f26888": "COMP",
    "0x0bc529c00C6401aEF6D220BE8C6Ea1667F6Ad93e": "YFI",
    "0x7D1AfA7B718fb893dB30A3aBc0Cfc608AaCfeBB0": "MATIC/POL",
}


@dataclass(frozen=True)
class Event:
    frm: str
    to: str
    amount: int
    block: int


def sh(*args: str) -> str:
    return subprocess.run(args, capture_output=True, text=True, check=True).stdout.strip()


def addr_from_topic(topic: str) -> str:
    # topic is 0x + 64 hex; address is last 40 hex
    return "0x" + topic[-40:].lower()


def cast_balance(token: str, who: str, block: int) -> int:
    # --json returns the bare ABI-decoded value (hex uint256), no [scientific] annotation
    out = sh(
        "cast", "call", token, "balanceOf(address)(uint256)", who,
        "--rpc-url", RP, "--block", str(block), "--json",
    )
    # cast --json on a single-return call yields a JSON array, e.g. ["1059…\"]
    try:
        val = json.loads(out)
        if isinstance(val, list):
            out = val[0]
        else:
            out = val  # bare number/str
    except json.JSONDecodeError:
        pass  # out is already a plain string
    if isinstance(out, int):
        return out
    out = str(out).strip().strip('"')
    if out.startswith("0x"):
        return int(out, 16)
    return int(out)


def main() -> None:
    latest = int(sh("cast", "block-number", "--rpc-url", RP))
    print(f"latest block = {latest}; scanning last {SCAN_BLOCKS} blocks per token\n")

    results: list[tuple[str, str, str]] = []  # (symbol, verdict, detail)

    # Merge whitelist + any CLI-provided addr:sym candidates.
    all_tokens = dict(TOKENS)
    all_tokens.update(_EXTRA)
    for token, symbol in all_tokens.items():
        if _SCAN_SYMBOLS and symbol not in _SCAN_SYMBOLS:
            continue
        frm_block = max(0, latest - SCAN_BLOCKS)
        raw = sh(
            "cast", "logs", "--rpc-url", RP, "--address", token,
            TRANSFER_TOPIC, "--from-block", str(frm_block), "--to-block", str(latest),
            "--json",
        )
        events: list[Event] = []
        if raw:
            try:
                blobs = json.loads(raw)
            except json.JSONDecodeError:
                blobs = []
            for b in blobs:
                if not isinstance(b, dict):
                    continue
                topics = b.get("topics") or []
                if len(topics) < 3:
                    continue
                data = b.get("data") or "0x"
                try:
                    amount = int(data, 16)
                except ValueError:
                    continue
                frm = addr_from_topic(topics[1])
                to = addr_from_topic(topics[2])
                block = int(b.get("blockNumber", "0x0"), 16)
                events.append(Event(frm, to, amount, block))

        # exclude mint/burn (0x0 endpoints) — those aren't user-to-user transfers
        user_events = [e for e in events if e.frm != ZERO and e.to != ZERO]
        # exclude transfers to burn/sink addresses (0x…dead etc.) — see BURN_SINKS
        user_events = [e for e in user_events if e.to not in BURN_SINKS]
        # exclude zero-amount transfers — they match vacuously (delta 0 == amount 0)
        # and prove nothing about a fee-on-transfer, so never accept them as proof.
        user_events = [e for e in user_events if e.amount > 0]

        # group inbound by (recipient, block); keep only singletons (no contamination)
        inbound: dict[tuple[str, int], list[Event]] = defaultdict(list)
        for e in user_events:
            inbound[(e.to, e.block)].append(e)
        clean_samples = [evs[0] for evs in inbound.values() if len(evs) == 1]

        verdict = "❌ NO TRANSFERS"
        detail = f"scan={len(events)} evts, {len(user_events)} user-to-user, 0 isolated"

        if not clean_samples:
            results.append((symbol, verdict, detail))
            print(f"{symbol:<11} {verdict}   ({detail})")
            continue

        for i, e in enumerate(clean_samples[:MAX_SAMPLES]):
            bal_before = cast_balance(token, e.to, e.block - 1)
            bal_after = cast_balance(token, e.to, e.block)
            delta = bal_after - bal_before
            if delta == e.amount:
                verdict = "✅ tax-free"
                detail = (f"block {e.block}: recipient {e.to[:10]}… received "
                          f"{e.amount} wei, balanceOf Δ={delta} (exact match)")
                break
            # mismatch — could be WETH deposit/withdrawal contamination or a real tax.
            # accumulate; if EVERY sample is shorted by a consistent ratio, it's a tax.
            if i == len(clean_samples) - 1 or i == MAX_SAMPLES - 1:
                # report the last sample we checked + summary
                ratio = (delta / e.amount) if e.amount else 0
                verdict = "⚠️  SUSPICIOUS"
                detail = (f"checked {min(i+1,MAX_SAMPLES)} samples; last@block {e.block}: "
                          f"received {e.amount}, Δ={delta} (ratio {ratio:.4f}) — "
                          f"contamination or tax; investigate")
        results.append((symbol, verdict, detail))
        print(f"{symbol:<11} {verdict}   ({detail})")

    print("\n=== SUMMARY ===")
    for symbol, verdict, _detail in results:
        print(f"  {symbol:<11} {verdict}")


if __name__ == "__main__":
    main()
