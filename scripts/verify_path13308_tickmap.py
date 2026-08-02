#!/usr/bin/env python3
"""Verify the fixture tick mapping against on-chain oracles at block 25664704.

V3 pools: `ticks(int24)` on the pool contract -> (liquidityGross, liquidityNet).
V4 pool:  `getTickLiquidity(bytes32,int24)` on the StateView -> (gross, net).

Also (completeness) uses TickLens `getPopulatedTicksInWord(address,int16)` to
enumerate populated ticks in the words the solver's swap crosses and reports any
populated tick that is MISSING from the fixture mapping.
"""
import json, os, sys, urllib.request

RPC = "http://host.containers.internal:8545"
TARGET = 25664704
FIX = "/workspaces/degenbot/tests/fixtures/path13308_v3v4v3_block25664704.json"

S_TICKS = "0xf30dba93"          # ticks(int24)
S_GTICK = "0xcaedab54"          # getTickLiquidity(bytes32,int24)
S_POPW = "0x351fb478"           # getPopulatedTicksInWord(address,int16)
S_BITMAP = "0x1c7ccb4c"         # getTickBitmap(bytes32,int16)
S_POOL_BITMAP = "0x5339c296"    # V3 pool tickBitmap(int16)

V3_0 = "0x60594a405d53811d3bc4766596efd80fd545a270"
V3_2 = "0x1ac1a8feaaea1900c4166deeed0c11cc10669d36"
SV = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"
V4_PID = "0xd967702f17f83d907b36e66c9a62eb50ac327432c581d5b273a76519692434be"
# Uniswap V3 mainnet TickLens
TICKLENS = "0xbfd8137f7d1516d3ea5ca83523914859ec47f573"


def rpc(method, params):
    req = urllib.request.Request(
        RPC, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
        headers={"Content-Type": "application/json"},
    )
    resp = json.load(urllib.request.urlopen(req, timeout=90))
    if "error" in resp:
        raise RuntimeError(f"{method}: {resp['error']}")
    return resp["result"]


def eth_call(to, data):
    return rpc("eth_call", [{"to": to, "data": data}, hex(TARGET)])[2:]


def i24(tick):
    return format(tick & 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF, "064x")


def i16(w):
    # int16 sign-extended to 32 bytes (negative -> all-Fs prefix)
    return format(w & 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF, "064x")


def addr(a):
    return a.lower()[2:].zfill(64)


def parse_2_words(hexdata):
    if len(hexdata) < 128:
        return None
    w0 = int(hexdata[:64], 16)
    w1 = int(hexdata[64:128], 16)
    net = w1 - (1 << 256) if w1 & (1 << 255) else w1
    return w0, net


def chain_v3_tick(pool, tick):
    data = S_TICKS + i24(tick)
    out = eth_call(pool, data)
    return parse_2_words(out)  # (gross u128, net i128)


def chain_v4_tick(tick):
    data = S_GTICK + V4_PID[2:] + i24(tick)
    out = eth_call(SV, data)
    return parse_2_words(out)


def check_pool(name, family, ticks):
    n_bad = 0; n_ok = 0; n_zero = 0; examples = []
    for tk in ticks:
        t = int(tk)
        exp_net = int(ticks[tk]["liquidity_net"]); exp_gross = int(ticks[tk]["liquidity_gross"])
        if family == "uniswap_v4":
            got = chain_v4_tick(t)
        else:
            pool = V3_0 if name == "v3_0" else V3_2
            got = chain_v3_tick(pool, t)
        if got is None:
            examples.append((t, "no-return")); n_bad += 1; continue
        gross, net = got
        if gross == 0 and net == 0:
            n_zero += 1
        if gross == exp_gross and net == exp_net:
            n_ok += 1
        else:
            n_bad += 1
            if len(examples) < 8:
                examples.append((t, f"chain=(g={gross},n={net}) fixture=(g={exp_gross},n={exp_net})"))
    print(f"{name}: ticks={len(ticks)} ok={n_ok} MISMATCH={n_bad} chain-zero={n_zero}")
    for t, e in examples:
        print(f"    tick {t}: {e}")
    return n_bad


def populated_ticks_in_word(pool, word):
    data = S_POPW + addr(pool) + i16(word)
    out = eth_call(TICKLENS, data)
    if len(out) < 64:
        return []
    # returns dynamic array: offset, len, then 3 arrays (tick[], net[], gross[])
    off = int(out[:64], 16)
    head = out[off * 2:]
    n = int(head[:64], 16)
    ticks = [int(head[64 + i * 64:64 + (i + 1) * 64], 16) for i in range(n)]
    return ticks


def v4_populated_word_bitmap(word):
    """StateView getTickBitmap(bytes32,int16) -> raw 256-bit word."""
    data = S_BITMAP + V4_PID[2:] + i16(word)
    out = eth_call(SV, data)
    return out[:64], int(out[:64], 16)


def v3_pool_bitmap(pool, word):
    """V3 pool contract `tickBitmap(int16)` -> raw 256-bit word."""
    data = S_POOL_BITMAP + i16(word)
    return int(eth_call(pool, data), 16)


def main():
    d = json.load(open(FIX))
    pools = d["pools"]
    total_bad = 0
    # --- per-tick byte match ---
    for name, p in pools.items():
        total_bad += check_pool(name, p["family"], p["tick_data"])
    print(f"\nTOTAL per-tick mismatches: {total_bad}")

    # --- completeness: populated ticks in words around the start tick ---
    print("\n=== completeness (TickLens populated vs fixture mapping) ===")
    for name, p in pools.items():
        fam = p["family"]
        ts = int(p["tick_spacing"])
        start = p["tick"]
        have = set(int(t) for t in p["tick_data"])
        word = start >> 8
        if fam == "uniswap_v4":
            mask, w = v4_populated_word_bitmap(word)
            pop = []
            # enumerate set bits within the word -> absolute ticks
            for bit in range(256):
                if w & (1 << bit):
                    pop.append((word << 8) + bit)
            missing = [pt for pt in pop if pt not in have]
            print(f"{name}: start_tick={start} word={word} populated={len(pop)} missing-from-fixture={missing}")
            print(f"    bitmap low16bits=...{mask[96:]}")
            continue
        pool_for = V3_0 if name == "v3_0" else V3_2
        spacing = int(p["tick_spacing"])
        have = set(int(t) for t in p["tick_data"])
        def trunc_div(a, b):
            return abs(a) // b * (1 if a >= 0 else -1)

        def word_of(t):
            return trunc_div(t, spacing) >> 8

        def bit_of(t):
            return trunc_div(t, spacing) % 256

        # group fixture ticks by their (compressed) bitmap word
        from collections import defaultdict
        by_word = defaultdict(list)
        for t in have:
            by_word[word_of(t)].append(t)
        missing_all = []
        extra_words = 0
        for w, ticks in sorted(by_word.items()):
            bm = v3_pool_bitmap(pool_for, w)
            chain_bits = {b for b in range(256) if bm & (1 << b)}
            fx_bits = {bit_of(t) for t in ticks}
            miss = chain_bits - fx_bits
            if miss:
                for b in miss:
                    missing_all.append((w, b))
            if fx_bits - chain_bits:
                extra_words += 1
        print(f"{name}: V3 word-bitmap check across {len(by_word)} populated words: "
              f"missing-chain-bits={len(missing_all)} "
              f"words-with-extra-fixture-bits={extra_words}")
        if missing_all:
            print(f"    MISSING (word,bit): {missing_all[:40]}")
        continue


if __name__ == "__main__":
    main()
