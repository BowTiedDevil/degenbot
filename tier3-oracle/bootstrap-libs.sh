#!/usr/bin/env bash
# Fetch the canonical Uniswap v2-core (v1.0.0) + v3-core (v1.0.0) + v4-core
# (v4.0.0) into `tier3-oracle/lib/`. These are gitignored (see .gitignore)
# and are the reference source the Tier-3 oracle harnesses compile — `git
# clone` (not `forge install`, which would record a submodule under a
# gitignored path) keeps them plain directories with no repo metadata to
# leak into the degenbot tree. Idempotent: skips a lib that already contains
# its pool contract. Run before `just test-tier3` on a fresh checkout (CI
# does this).
set -euo pipefail
TD="$(cd "$(dirname "$0")" && pwd)"
cd "${TD}"
mkdir -p lib

ensure_lib() {  # ensure_lib <name> <repo-url> <tag> <marker-file>
    local name="$1" url="$2" tag="$3" marker="lib/$1/$4"
    if [ -f "${marker}" ]; then
        echo "lib/${name}: present ($(head -c 12 "lib/${name}/.git" 2>/dev/null || echo cloned)), skipping"
        return 0
    fi
    echo "lib/${name}: cloning ${url}@${tag}…"
    rm -rf "lib/${name}"
    git clone --depth 1 --branch "${tag}" "${url}" "lib/${name}"
    rm -rf "lib/${name}/.git"
}

ensure_lib v2-core https://github.com/Uniswap/v2-core.git v1.0.0 contracts/UniswapV2Pair.sol
ensure_lib v3-core https://github.com/Uniswap/v3-core.git v1.0.0 contracts/UniswapV3Pool.sol
ensure_lib v4-core https://github.com/Uniswap/v4-core.git v4.0.0 src/PoolManager.sol

# NOTE: the Curve stableswap oracle (`src-curve/CurveSwapOracleHarness.sol`, task
# YXMNWB) has NO `ensure_lib` entry: Curve's canonical source is VYPER (not
# compilable in this env — no vyper toolchain), so the harness is a faithful
# Solidity 0.8.26 port of the STANDARD `get_dy` and is itself the on-chain
# reference (a documented toolchain deviation). Its build is self-contained via
# `build-tier3-curve-swap-harness.sh` (direct solc 0.8.26, no imports).

# v4-core's ProtocolFees imports solmate's `Owned.sol`; the depth-1 clone above
# drops submodules, so vendor solmate separately (needed by the V4 swap oracle
# harness, 2LTKVO).
ensure_lib v4-core/lib/solmate https://github.com/transmissions11/solmate.git master src/auth/Owned.sol

# Balancer math cores (FixedPoint/LogExpMath/WeightedMath/StableMath + their
# FixedPoint imports) for the Balancer tier-3 oracle (task EZLECC). The
# canonical sources are Solidity (not Vyper), so we vendor the exact files at a
# pinned balancer-v2-monorepo commit under `lib/balancer-src/` preserving their
# `@balancer-labs/v2-solidity-utils` / `@balancer-labs/v2-interfaces` import
# paths. Idempotent: skips a file that already exists.
BALANCER_PIN="f8b6f44f21afaf3c802536ed478277f945e7f256"
BALANCER_RAW="https://raw.githubusercontent.com/balancer/balancer-v2-monorepo/${BALANCER_PIN}"
ensure_balancer_ref() {  # ensure_balancer_ref <package-rel-path> <local-rel-path>
    local url="${BALANCER_RAW}/$1"
    local dest="lib/balancer-src/$2"
    if [ -f "${dest}" ]; then
        echo "balancer-src/$2: present, skipping"
        return 0
    fi
    echo "balancer-src/$2: fetching ${url}"
    mkdir -p "$(dirname "${dest}")"
    curl -fsSL -o "${dest}" "${url}"
}
ensure_balancer_ref pkg/solidity-utils/contracts/math/LogExpMath.sol                  solidity-utils/contracts/math/LogExpMath.sol
ensure_balancer_ref pkg/solidity-utils/contracts/math/FixedPoint.sol                 solidity-utils/contracts/math/FixedPoint.sol
ensure_balancer_ref pkg/solidity-utils/contracts/math/Math.sol                       solidity-utils/contracts/math/Math.sol
ensure_balancer_ref pkg/solidity-utils/contracts/helpers/InputHelpers.sol            solidity-utils/contracts/helpers/InputHelpers.sol
ensure_balancer_ref pkg/pool-weighted/contracts/WeightedMath.sol                     solidity-utils/contracts/math/WeightedMath.sol
ensure_balancer_ref pkg/pool-stable/contracts/StableMath.sol                         solidity-utils/contracts/math/StableMath.sol
ensure_balancer_ref pkg/interfaces/contracts/solidity-utils/helpers/BalancerErrors.sol interfaces/contracts/solidity-utils/helpers/BalancerErrors.sol
ensure_balancer_ref pkg/interfaces/contracts/solidity-utils/openzeppelin/IERC20.sol  interfaces/contracts/solidity-utils/openzeppelin/IERC20.sol

# PancakeSwap V3 pool reference source (task: PancakeSwap V3 variant harness).
# Unlike the Uniswap forks, there is no public git ref matching the DEPLOYED
# PancakeV3Pool exactly, so we pull the Etherscan-verified source of a real
# deployed pool (WETH/USDC, fee 100, 0x1445..12b31 — solc 0.7.6) and extract
# the solc-input files into `lib/pancake-src/` (gitignored). Requires
# `ETHERSCAN_API_KEY` (a CI secret for the tier3-oracle job) only when the
# source is not already vendored.
PANCAKE_POOL="0x1445F32D1A74872bA41f3D8cF4022E9996120b31"
ensure_pancake_v3() {
    local marker="lib/pancake-src/contracts/PancakeV3Pool.sol"
    if [ -f "${marker}" ]; then
        echo "pancake-src: present (PancakeV3Pool.sol), skipping"
        return 0
    fi
    if [ -z "${ETHERSCAN_API_KEY:-}" ]; then
        echo "ERROR: ETHERSCAN_API_KEY is required to vendor the PancakeSwap V3 source (not present)." >&2
        echo "  Set ETHERSCAN_API_KEY (Etherscan V2 API key) to fetch the verified PancakeV3Pool source." >&2
        exit 1
    fi
    echo "pancake-src: fetching verified PancakeV3Pool source from Etherscan (pool ${PANCAKE_POOL})…"
    local tmp
    tmp="$(mktemp)"
    curl -fsSL "https://api.etherscan.io/v2/api?chainid=1&module=contract&action=getsourcecode&address=${PANCAKE_POOL}&apikey=${ETHERSCAN_API_KEY}" -o "${tmp}"
    python3 - "${tmp}" <<'PY'
import json, os, sys
raw=json.load(open(sys.argv[1]))
assert raw.get("status")=="1", f"Etherscan getSourceCode failed: {raw.get('result')}"
src=(raw["result"][0].get("SourceCode") or "")
assert src, "empty SourceCode"
obj=json.loads(src[1:-1]) if src.startswith("{{") else json.loads(src)
for path,c in obj["sources"].items():
    # Rewrite the @pancakeswap/v3-lm-pool import to the vendored tree path.
    rel=path
    if rel.startswith("@pancakeswap/v3-lm-pool/"):
        rel="v3-lm-pool/"+rel[len("@pancakeswap/v3-lm-pool/"):]
    dest=os.path.join("lib/pancake-src", rel)
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    open(dest,"w").write(c["content"])
print(f"wrote {len(obj['sources'])} PancakeSwap source files to lib/pancake-src/")
PY
    rm -f "${tmp}"
}
ensure_pancake_v3

echo "tier3-oracle libs ready: v2-core@v1.0.0, v3-core@v1.0.0, v4-core@v4.0.0 (with solmate), balancer-src@${BALANCER_PIN}, pancake-src@${PANCAKE_POOL}"
