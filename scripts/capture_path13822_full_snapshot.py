#!/usr/bin/env python3
"""THOROUGH block-consistent snapshot of all three path-13822 pools at 25696004.

For every pool, enumerate the COMPLETE initialized-tick map ON-CHAIN (via the
pool's `tickBitmap(int16)` + `ticks(int24)` getters) and read live scalars —
NOT trusting the static DB snapshot. This gives an internally-consistent state
at the solve block, ready for a faithful solver-fixture repro.

Route WETH->DAI->USDC->WETH; hop1 (DAI/USDC `0x5777d92f`, fee 100, spacing 1)
is the 1-wei over-prediction.
"""
import urllib.request, json, os
RPC="http://host.containers.internal:8545"
BLK=25696004
BLOCK_H="0x%x"%BLK
P=BOOL={}
POOLS={
  "v3_0":{"addr":"0x60594a405d53811d3bc4766596efd80fd545a270","family":"uniswap_v3","tick_spacing":10},
  "v3_1":{"addr":"0x5777d92f208679db4b9778590fa3cab3ac9e2168","family":"uniswap_v3","tick_spacing":1},
  "v3_2":{"addr":"0x1445f32d1a74872ba41f3d8cf4022e9996120b31","family":"pancakeswap_v3","tick_spacing":1},
}
def rpc(m,p):
    req=urllib.request.Request(RPC,data=json.dumps({"jsonrpc":"2.0","id":1,"method":m,"params":p}).encode(),headers={"Content-Type":"application/json"})
    j=json.load(urllib.request.urlopen(req,timeout=60))
    if "error" in j: raise RuntimeError(j["error"])
    return j["result"]
def call(pool,sig,arg):
    # 32-byte argument, sign-extend ints
    v=arg if arg>=0 else (1<<256)+arg
    data=sig+("%064x"%v)
    out=rpc("eth_call",[{"to":pool,"data":data},BLOCK_H])
    return out
SEL_TICKS="0xf30dba93"  # ticks(int24)
SEL_BM="0x5339c296"     # tickBitmap(int16)
import math
def word_of(t,spacing):
    comp=t//spacing
    return math.floor(comp/256) if comp>=0 else -((-(comp)+255)//256)
def ticks_in_word(w,spacing):
    base=w*256*spacing
    return [base+i*spacing for i in range(256)]
def initialized_ticks(pool,spacing):
    lo=word_of(-887272,spacing); hi=word_of(887272,spacing)
    ticks=[]
    for w in range(lo,hi+1):
        bm=int(call(pool,SEL_BM,w),16)
        if not bm: continue
        wbase=w*256*spacing
        for i in range(256):
            if (bm>>i)&1:
                ticks.append(wbase+i*spacing)
    return ticks
def tick_info(pool,tick):
    out=call(pool,SEL_TICKS,tick)
    words=[out[i:i+64] for i in range(2,len(out)-1,64)]  # skip 0x
    lg=int(words[0],16); ln=int(words[1],16)
    # int128 two's complement
    if ln & (1<<255): ln-= (1<<256)  # int256 sign-extend
    return lg, ln

result={}
for key,p in POOLS.items():
    addr=p["addr"]; sp=p["tick_spacing"]
    tks=initialized_ticks(addr,sp)
    tdata={}
    for t in tks:
        lg,ln=tick_info(addr,t)
        tdata[t]={"liquidity_gross":lg,"liquidity_net":ln}
    # scalars
    slot0=call(addr,"0x3850c7bd",0)
    w=[slot0[i:i+64] for i in range(2,len(slot0)-1,64)]
    sqrt=int(w[0],16); tick=int(w[1],16)
    if tick & (1<<255): tick-=1<<256  # int256 sign-extend
    liq=int(call(addr,"0x1a686502",0),16)
    # tokens + fee
    t0=call(addr,"0x0dfe1681",0)[2:]
    t1=call(addr,"0xd21220a7",0)[2:]
    fee=int(call(addr,"0xddca3f43",0),16)
    result[key]={
      "family":p["family"],"address":addr,"token0":"0x"+t0,"token1":"0x"+t1,
      "tick_spacing":sp,"fee":fee,
      "sqrt_price_x96":sqrt,"tick":tick,"liquidity":liq,
      "n_ticks":len(tks),"tick_data":tdata,
    }
    print(f"{key}: {addr[:10]}.. ticks_onchain={len(tks)} sqrt={sqrt} tick={tick} liq={liq} fee={fee}")
out="/workspaces/degenbot/tests/fixtures/path13822_v3v3v3_block25696004_onchain.json"
json.dump({"target_block":BLK,"pools":result},open(out,"w"),indent=1)
print("wrote",out)
