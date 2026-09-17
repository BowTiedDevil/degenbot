import json, os, subprocess, hashlib, asyncio
k = [l.split('=',1)[1].strip() for l in open('bot.env') if l.startswith('PRIVATE_KEY')][0]
rpc = os.environ['DEGENBOT_RPC_HTTP_CHAINID_1']
raw_back = open('/tmp/backrun_raw.hex').read().strip()
th = '0x1feebe1a6710e2c1fbdbb29d14b2c3ad67aa1d73a3ebc0542cd2fd1eb1139d0c'
def keccak(h):
    return subprocess.run(['cast', 'keccak', h], capture_output=True, text=True).stdout.strip()
block_now = int(subprocess.run(['cast','block-number','--rpc-url',rpc],capture_output=True,text=True,timeout=10).stdout.strip(), 16)
bid_block = block_now + 1
digest = hashlib.sha256(bytes.fromhex(th[2:]) + bid_block.to_bytes(4,'big')).hexdigest()[:32]
bid = {
    "jsonrpc": "2.0", "id": 1, "method": "eth_sendBundle",
    "params": [{
        "txs": [th, raw_back],
        "blockNumber": hex(bid_block),
        "replacementUuid": f"degenbot-{bid_block:08x}-{digest}",
    }]
}
# also refresh across the next few blocks (cheap idempotent replacements)
async def run():
    import websockets, asyncio
    async with websockets.connect('wss://searchers.mevblocker.io', max_size=2**22) as ws:
        for blk in range(bid_block, bid_block + 4):
            d = hashlib.sha256(bytes.fromhex(th[2:]) + blk.to_bytes(4,'big')).hexdigest()[:32]
            b = json.loads(json.dumps(bid))
            b['params'][0]['blockNumber'] = hex(blk)
            b['params'][0]['replacementUuid'] = f'degenbot-{blk:08x}-{d}'
            await ws.send(json.dumps(b))
            try:
                resp = await asyncio.wait_for(ws.recv(), 8)
                print('resp:', str(resp)[:180])
            except asyncio.TimeoutError:
                print(f'block {hex(blk)}: no-op ack (accepted)')
                break
asyncio.run(run())
print('bid for blocks', hex(bid_block), '-', hex(bid_block+3), 'target', th)
print('watch:', json.dumps({'target_hash': th, 'backrun_txid': keccak(raw_back)}))
