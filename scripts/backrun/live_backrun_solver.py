from eth_account import Account
import asyncio, json, os, time, hashlib, sys
import websockets, requests
from eth_abi import encode as abi_encode, decode as abi_decode
from eth_utils import keccak

RPC = os.environ["DEGENBOT_RPC_HTTP_CHAINID_1"]
STREAM = "wss://searchers.mevblocker.io"
SUB = "mevblocker_partialPendingTransactions"
EXEC = "0x30b28ed8aa581fbc0191c3b532b0697773070e97"
Q96 = 2 ** 96
FEE_DEN = 10 ** 6
BUDGET_WEI = 200_000_000_000_000          # 0.0002 ETH risk per candidate
WETH = "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
USDC = "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
USDT = "dac17f958d2ee523a2206206994597c13d831ec7"
DAI = "6b175474e89094c44da98b954eedeac495271d0f"
POOLS = [
    ('0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640', 500, USDC, WETH),
    ('0x8ad599c3a0ff1de082011efddc58f1908eb6e6d8', 3000, USDC, WETH),
    ('0x11b815efb8f581194ae79006d24e0d814b7697f6', 500, WETH, USDT),
    ('0x4e68ccd3e89f51c3074ca5072bbac773960dfa36', 3000, WETH, USDT),
    ('0x60594a405d53811d3bc4766596efd80fd545a270', 500, DAI, WETH),
    ('0xc2e9f25be6257c210d7adf0d4cd6e3e881ba25f8', 3000, DAI, WETH),
]
BY_ADDR = {p[0]: p for p in POOLS}
SEEN = set()          # our own txids to ignore
SUBMITTED = []

def log(*a):
    print(time.strftime("[%H:%M:%S]"), *a, flush=True)

def rpc(method, params):
    j = requests.post(RPC, json={"jsonrpc": "2.0", "id": 3, "method": method, "params": params}, timeout=12).json()
    if "error" in j:
        raise RuntimeError(f"{method}: {str(j['error'])[:150]}")
    return j["result"]

def call(to, data):
    return rpc("eth_call", [{"to": to, "data": data}, "latest"])

def toks(pool):
    t0 = "0x" + call(pool, "0x0dfe1681")[26:66]
    t1 = "0x" + call(pool, "0xd21220a7")[26:66]
    return t0.lower(), t1.lower()

def _word(b, i):
    raw = bytes.fromhex(b[2:])
    return int.from_bytes(raw[i * 32:(i + 1) * 32], 'big')

def slot0(pool):
    d = call(pool, "0x3850c7bd")
    return _word(d, 0), _word(d, 3)

def liquidity(pool):
    return _word(call(pool, "0x1a686502"), 0)

def v3_exact_in(sqrtP, L, fee, amt_in, in0):
    amt = amt_in * (FEE_DEN - fee) // FEE_DEN
    if in0:
        num, den = L * sqrtP * Q96, L * Q96 + amt * sqrtP
        sp2 = num // den
        out = L * (sqrtP - sp2) // Q96
    else:
        sp2 = sqrtP + (amt << 96) // L
        out = L * (sp2 - sqrtP) // Q96
    return out, sp2

def config_word(bips):
    return 1 | (bips << 8) | (0 << 24)

def exec_calldata(stream, config):
    sel = keccak(text="execute(bytes,uint256)")[:4].hex()
    return "0x" + sel + abi_encode(["bytes", "uint256"], [stream, config]).hex()

def sim(calls):
    j = requests.post(RPC, json={"jsonrpc": "2.0", "id": 4, "method": "eth_simulateV1", "params": [
        {"blockStateCalls": [{"calls": calls}]}]}, timeout=15).json()
    if "error" in j:
        return {"error": str(j["error"])[:160]}
    return j["result"][0]

def sanity():
    p5, p3 = POOLS[0], POOLS[1]
    sp5 = slot0(p5[0])[0]; L5 = liquidity(p5[0])
    sp3 = slot0(p3[0])[0]; L3 = liquidity(p3[0])
    log(f"sanity: 500bp sqrtP={sp5} L={L5} | 3000bp sqrtP={sp3} L={L3}")
    X = BUDGET_WEI
    in0_5 = (p5[2] == WETH)
    out1, _ = v3_exact_in(sp5, L5, p5[1], X, in0_5)
    in0_3 = (p3[2] == USDC)
    gross, _ = v3_exact_in(sp3, L3, p3[1], out1 - 5, in0_3)
    stream = (b"\x00" + bytes.fromhex(p5[0][2:]) + b"\x00" + bytes.fromhex(p3[0][2:]) + b"\xff"
              + bytes([0x30, 0, 1 if in0_5 else 0]) + X.to_bytes(12, "big") + b"\xfd\x00"
              + bytes([0x30, 1, 1 if in0_3 else 0]) + (out1 - 5).to_bytes(12, "big") + b"\xfd\x00")
    cd = exec_calldata(stream, config_word(0))
    res = sim([{"from": OP, "to": EXEC, "data": cd, "gas": hex(600_000)}])
    ok = bool(res["calls"][0]["status"]) if not res.get("error") else False
    gasUsed = int(res["calls"][0]["gasUsed"], 16) if not res.get("error") else 0
    ret = res["calls"][0].get("returnData", "0x") if not res.get("error") else ""
    profit = int.from_bytes(bytes.fromhex(ret[2:]), "big") if ret and ret != "0x" else 0
    log(f"sanity: est_gross={gross} sim_ok={ok} gasUsed={gasUsed} profit={profit}")
    return ok

def parse_swap_logs(logs):
    """Return [(addr, post_sqrtP_or_None)] for v3 Swap events we can act on."""
    out = []
    for lg in logs:
        t = (lg.get("topics") or [""])[0].lower()
        if t.startswith("0xc42079f9") and lg.get("address","").lower() in BY_ADDR:
            d = lg["data"][2:]
            if len(d) >= 192:
                sp = int(d[0:64], 16)
                out.append((lg["address"].lower(), sp))
    return out

def candidate_for(pool_a, sp_post):
    """Direction + leg amounts vs the sibling-tier responsive pool."""
    meta = BY_ADDR[pool_a]
    _, t0, t1 = meta[0], meta[2], meta[3]
    sibling = [p for p in POOLS if p[0] != pool_a and {p[2], p[3]} == {t0, t1}]
    if not sibling:
        return None
    pb = sibling[0]
    sp_b, L_b = slot0(pb[0])
    L_a = liquidity(pool_a)
    # which side of pool A got cheaper vs B?
    # token0/token1 relative: if t0==WETH: cheap0 = A sells WETH cheap
    if t0 == WETH:
        a_cheap = sp_post < sp_b * 9995 // 10000
        a_rich = sp_post > sp_b * 10005 // 10000
    else:
        a_cheap = sp_post > sp_b * 10005 // 10000
        a_rich = sp_post < sp_b * 9995 // 10000
    if not (a_cheap or a_rich):
        return None
    zfo_a_to_b = None
    # determine zfo flags for WETH->token and token->WETH on each pool
    def zfo_weth_in(p):
        return (p[2] == WETH)          # in WETH (=token0) -> zfo True
    def zfo_token_in(p):
        return (p[2] != WETH)          # in token (=token0 if WETH is token1) -> zfo False when WETH is token1
    if a_rich:
        # leg1: WETH->token in A (sell high), leg2: token->WETH in B
        pool1, pool2 = pool_a, pb[0]
        zfo1, zfo2 = zfo_weth_in(BY_ADDR[pool_a]), zfo_token_in(pb)
        fee1 = meta[1]
    else:
        # leg1: WETH->token in B (fair), leg2: token->WETH in A (buy cheap)
        pool1, pool2 = pb[0], pool_a
        zfo1, zfo2 = zfo_weth_in(pb), zfo_token_in(BY_ADDR[pool_a])
        fee1 = pb[1]
    sp1 = slot0(pool1)[0]
    L1 = liquidity(pool1)
    out1, _ = v3_exact_in(sp1, L1, fee1, BUDGET_WEI, (pool1 and (BY_ADDR[pool1][2] == WETH)))
    fee2 = BY_ADDR[pool2][1]
    sp2p, L2 = (sp_post, L_a) if pool2 == pool_a else (sp_b, L_b)
    in0_2 = (BY_ADDR[pool2][2] != WETH)
    gross, _ = v3_exact_in(sp2p, L2, fee2, out1 - 5, in0_2)
    return (pool1, pool2, zfo1, zfo2, out1 - 5, gross)

def process_frame(th, tx):
    # 1) target solo sim at head: what does it do?
    solo = sim([{k: tx[k] for k in ("from", "to", "value", "data", "gas") if k in tx}])
    if "error" in solo or not bool(solo["calls"][0]["status"]):
        return log(f"{th[:14]} target sim: {'err' if 'error' in solo else 'revert'} - skip")
    logs = solo["calls"][0].get("logs", [])
    swappable = parse_swap_logs(logs)
    if not swappable:
        return log(f"{th[:14]} ok, no actionable v3 swap ({len(logs)} logs) - observe")
    for pool_a, sp_post in swappable:
        log(f"{th[:14]} affected {pool_a[:14]} post_sqrtP={sp_post}")
        c = candidate_for(pool_a, sp_post)
        if not c:
            continue
        pool1, pool2, zfo1, zfo2, amt2, gross = c
        stream = (b"\x00" + bytes.fromhex(pool1[2:]) + b"\x00" + bytes.fromhex(pool2[2:]) + b"\xff"
                  + bytes([0x30, 0, 1 if zfo1 else 0]) + BUDGET_WEI.to_bytes(12, "big") + b"\xfd\x00"
                  + bytes([0x30, 1, 1 if zfo2 else 0]) + amt2.to_bytes(12, "big") + b"\xfd\x00")
        target_call = {k: tx[k] for k in ("from", "to", "value", "data", "gas") if k in tx}
        # 2) bundle sim: [target, backrun], no bribe yet
        res = sim([target_call, {"from": OP, "to": EXEC, "data": exec_calldata(stream, config_word(0)), "gas": hex(600_000)}])
        if "error" in res or not bool(res["calls"][-1]["status"]):
            return log(f"{th[:14]} bundle sim: {'err' if 'error' in res else 'revert'} - skip")
        back = res["calls"][-1]
        gas_used = int(back.get("gasUsed", "0x0"), 16)
        ret = back.get("returnData", "0x")
        gross_actual = int.from_bytes(bytes.fromhex(ret[2:]), "big") if ret and ret != "0x" else 0
        # 3) gas cost + bribe sizing
        blk = rpc("eth_getBlockByNumber", ["latest", False])
        base_fee = int(blk["baseFeePerGas"], 16)
        tip = 1_000_000_000
        gas_cost = gas_used * (base_fee * 12 // 10 + tip)
        net = gross_actual - gas_cost
        if net <= 0:
            return log(f"{th[:14]} gross={gross_actual} gas={gas_cost}: net<=0 - skip")
        bips = max(1, 10_000 * net // 10 // max(gross_actual, 1))
        cd = exec_calldata(stream, config_word(bips))
        # 4) access list
        al = rpc("eth_createAccessList", [{"from": OP, "to": EXEC, "data": cd, "gas": hex(600_000)},
                                          "latest"])
        # 5) final bundle sim with access list + bribe bips
        fin_call = {"from": OP, "to": EXEC, "data": cd, "gas": hex(600_000), "accessList": al["accessList"]}
        fin = sim([target_call, fin_call])
        ok = not fin.get("error") and bool(fin["calls"][-1]["status"])
        if not ok:
            return log(f"{th[:14]} final sim failed ({str(fin)[:80]})")
        gas_used_f = int(fin["calls"][-1].get("gasUsed", "0x0"), 16)
        net_f = gross_actual - gas_used_f * (base_fee * 12 // 10 + tip)
        log(f"{th[:14]} VALID: bribe_bips={bips} gross={gross_actual} gasUsed={gas_used_f} net~{net_f}")
        return (th, tx, cd, fin_call, gas_used_f, base_fee)

def sign_and_submit(th, cd, al, nonce, base_fee, blk_target, ws):
    from eth_account import Account
    tx = {"type": 2, "chainId": 1, "nonce": nonce, "gas": 600_000,
          "maxFeePerGas": base_fee * 12 // 10 + 1_000_000_000,
          "maxPriorityFeePerGas": 1_000_000_000,
          "to": EXEC, "data": cd, "accessList": al}
    signed = Account.sign_transaction(tx, os.environ["PRIVATE_KEY"])
    raw = signed.raw_transaction.hex() if hasattr(signed, "raw_transaction") else signed.rawTransaction.hex()
    digest = hashlib.sha256(bytes.fromhex(th[2:]) + blk_target.to_bytes(4, "big")).hexdigest()[:32]
    bid = {"jsonrpc": "2.0", "id": 1, "method": "eth_sendBundle",
           "params": [{"txs": [th, "0x" + raw if not raw.startswith("0x") else raw],
                       "blockNumber": hex(blk_target),
                       "replacementUuid": f"degenbot-{blk_target:08x}-{digest}"}]}
    ws.send(json.dumps(bid))
    try:
        resp = asyncio.wait_for(ws.recv(), 8)
        log(f"submit resp: {str(resp)[:120]}")
    except Exception:
        log(f"submit: no-op ack block {hex(blk_target)}")

OP = Account.from_key(os.environ['PRIVATE_KEY']).address

async def run():
    global glob_nonce
    import web3
    from web3 import Web3
    w3 = Web3(Web3.HTTPProvider(RPC))
    nonce = w3.eth.get_transaction_count(OP)
    if not sanity():
        log("sanity FAILED - aborting run")
        return
    log("sanity OK - live loop starting")
    async with websockets.connect(STREAM, max_size=2**22) as ws:
        await ws.send(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "eth_subscribe", "params": [SUB]}))
        await asyncio.wait_for(ws.recv(), 10)
        deadline = asyncio.get_event_loop().time() + int(os.environ.get("RUN_SECS", "900"))
        while asyncio.get_event_loop().time() < deadline:
            try:
                msg = json.loads(await asyncio.wait_for(ws.recv(), timeout=10))
            except asyncio.TimeoutError:
                continue
            tx = msg.get("params", {}).get("result", {})
            th = tx.get("hash")
            if not isinstance(tx, dict) or not th or th in SEEN:
                continue
            SEEN.add(th)
            try:
                cand = process_frame(th, tx)
                if isinstance(cand, tuple):
                    th, tx, cd, fin_call, gas_used_f, base_fee = cand
                    al = rpc("eth_getBlockByNumber", ["latest", False]) and None
                    # fetch access list properly
                    al_res = rpc("eth_createAccessList", [{"from": OP, "to": EXEC, "data": cd, "gas": hex(600_000)}, "latest"])
                    blk_now = w3.eth.block_number
                    sign_and_submit(th, cd, al_res["accessList"], nonce, base_fee, blk_now + 1, ws)
                    nonce += 1
                    if len(SUBMITTED) >= 2:
                        log("submission cap reached - observe-only from here")
            except Exception as e:
                log(f"frame error {th[:14]}: {str(e)[:140]}")
        log("run window complete")

asyncio.run(run())
