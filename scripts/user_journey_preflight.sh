#!/usr/bin/env bash
# user_journey_preflight.sh — mechanical launch-checklist for the autonomous
# user-journey exercise (docs/autonomous-user-journey/). Exits 0 only when the
# environment is safe+ready to hand to the agent. Never prints key material.
# Usage: scripts/user_journey_preflight.sh [min_balance_eth]
set -u
cd "$(dirname "$0")/.."
# Floor is deploy-dominated (3.6M gas): 0.005 ETH suffices with base fee
# <= ~1.3 gwei; the [FAIL] threshold defaults to that floor, and the base-fee
# check below gates the deploy window.
MIN_ETH="${1:-0.005}"
FAIL=0
ok()   { printf '[OK]   %s\n' "$1"; }
bad()  { printf '[FAIL] %s\n' "$1"; FAIL=1; }
warn() { printf '[WARN] %s\n' "$1"; }

RPC="${DEGENBOT_RPC_HTTP_CHAINID_1:-}"
WS="${DEGENBOT_RPC_WS_CHAINID_1:-}"
[ -n "$RPC" ] && ok "http endpoint set" || bad "DEGENBOT_RPC_HTTP_CHAINID_1 unset"
[ -n "$WS" ]  && ok "ws endpoint set"   || bad "DEGENBOT_RPC_WS_CHAINID_1 unset"

# 1. chain id + liveness
CID=$(cast chain-id --rpc-url "$RPC" 2>/dev/null)
[ "$CID" = "1" ] && ok "chain-id 1" || bad "chain-id '$CID' (expected 1)"

# 2. blocks advancing
B0=$(cast block-number --rpc-url "$RPC" 2>/dev/null || echo 0)
sleep 14
B1=$(cast block-number --rpc-url "$RPC" 2>/dev/null || echo 0)
[ "$B1" -gt "$B0" ] && ok "blocks advancing ($B0 -> $B1)" || bad "blocks NOT advancing ($B0 -> $B1)"

# 3. funded key (address derived without printing the key)
OP=$(cast wallet address --private-key "$(grep -E '^PRIVATE_KEY=' bot.env | cut -d= -f2)" 2>/dev/null)
BAL=$(cast balance "$OP" --rpc-url "$RPC" 2>/dev/null || echo 0)
MIN_WEI=$(cast to-wei "$MIN_ETH" eth)
if [ "$(python3 -c "print(1 if $BAL >= $MIN_WEI else 0)")" = "1" ]; then
  ok "operator $OP funded ($(cast from-wei "$BAL" 2>/dev/null) ETH >= $MIN_ETH)"
else
  bad "operator $OP under-funded ($(cast from-wei "$BAL" 2>/dev/null || echo "$BAL wei") ETH < $MIN_ETH)"
fi

# 4. no bot currently running
if ./run_bot.sh status 2>/dev/null | grep -q "^\[runner\] running"; then
  bad "a bot instance is already running (./run_bot.sh status)"
else
  ok "no bot running"
fi

# 5. build freshness (installed .so matches sources)
if uv run --no-sync python -m degenbot.build_info >/dev/null 2>&1; then
  ok "build fresh (receipt matches)"
else
  bad "stale build — run: just dev"
fi

# 6. DB warm + gap modest
DB="$HOME/.config/degenbot/degenbot.db"
if [ -f "$DB" ]; then
  SNAP=$(sqlite3 "$DB" "SELECT MAX(liquidity_update_block) FROM uniswap_v4_pools;" 2>/dev/null || echo 0)
  GAP=$((B1 - ${SNAP:-0}))
  [ "$GAP" -lt 50000 ] && ok "DB snapshot gap $GAP blocks (< 50k)" || warn "DB snapshot gap $GAP blocks (boot backfill will be slow)"
else
  warn "no DB at $DB — the agent will need to bootstrap pool snapshots"
fi

# 6b. base fee window (deploy is the bankroll-dominant cost)
BF=$(cast base-fee --rpc-url "$RPC" 2>/dev/null || echo 0)
DEPLOY_GAS=3595884
if [ "$BF" != "0" ]; then
  DEPLOY_WEI=$(python3 -c "print($DEPLOY_GAS * $BF)")
  DEPLOY_ETH=$(cast from-wei "$DEPLOY_WEI" 2>/dev/null)
  # warn if deploy alone would exceed the funded floor + leave < 0.002 buffer
  [ "$(python3 -c "print(1 if $BF <= 1_300_000_000 else 0)")" = "1" ] \
    && ok "base fee $(python3 -c "print(round($BF/1e9,3))") gwei — deploy ~$DEPLOY_ETH ETH (window open)" \
    || warn "base fee $(python3 -c "print(round($BF/1e9,3))") gwei — deploy ~$DEPLOY_ETH ETH; wait for <= ~1.3 gwei"
fi

# 7. relay allowlist liveness (read-only chain-id probes)
for ep in \
  "https://rpc.flashbots.net?hint=hash" \
  "https://rpc.mevblocker.io/noreverts" \
  "https://rpc.mevblocker.io/fullprivacy" ; do
  R=$(curl -s -m 8 -X POST -H 'Content-Type: application/json' \
    --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' "$ep" \
    | grep -o '"0x1"' || true)
  [ "$R" = '"0x1"' ] && ok "relay live: $ep" || bad "relay dead/wrong chain: $ep"
done

# 8. archive read sanity (fork-replay credit needs historical eth_call)
TS=$(cast call 0xC02aaA39b223Fe8D0A0e5C4f27eAD9083C756Cc2 'totalSupply()(uint256)' \
  --rpc-url "$RPC" --block $((B1 > 5000 ? B1 - 5000 : 1)) 2>/dev/null || echo "")
[ -n "$TS" ] && ok "historical eth_call works at tip-5000" || bad "historical eth_call FAILED (fork-replay credit impossible)"

# 9. no leftover STOP file
[ -f STOP ] && bad "STOP file present at repo root — remove before launch" || ok "no STOP file"

echo
[ "$FAIL" = "0" ] && { echo "PREFLIGHT PASS"; exit 0; } || { echo "PREFLIGHT FAIL — resolve [FAIL] items before launch"; exit 1; }
