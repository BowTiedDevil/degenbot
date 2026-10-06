"""The BalancerQueries ``querySwap`` ABI fragment the on-chain parity tests
encode their swap calldata with.
"""

from __future__ import annotations

from typing import Any

BALANCERQUERIES_ABI: list[dict[str, Any]] = [
    {
        "inputs": [
            {
                "components": [
                    {"name": "poolId", "type": "bytes32"},
                    {"name": "kind", "type": "uint8"},
                    {"name": "assetIn", "type": "address"},
                    {"name": "assetOut", "type": "address"},
                    {"name": "amount", "type": "uint256"},
                    {"name": "userData", "type": "bytes"},
                ],
                "internalType": "struct IVault.SingleSwap",
                "name": "singleSwap",
                "type": "tuple",
            },
            {
                "components": [
                    {"name": "sender", "type": "address"},
                    {"name": "fromInternalBalance", "type": "bool"},
                    {"name": "recipient", "type": "address"},
                    {"name": "toInternalBalance", "type": "bool"},
                ],
                "internalType": "struct IVault.FundManagement",
                "name": "funds",
                "type": "tuple",
            },
        ],
        "name": "querySwap",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "nonpayable",
        "type": "function",
    },
]
