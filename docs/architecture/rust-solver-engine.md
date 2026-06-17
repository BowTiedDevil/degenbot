# Rust Solver Engine — Three-Layer Architecture

Four concerns, three layers. Implementation details (log decoders, `tick_bitmap.rs`,
event buffers, `SnapshotStore`, shared infra, on-chain verification, dual-orientation
registration) are encapsulated inside the boxes they belong to rather than drawn
separately. See `rust/CONTEXT.md` for term definitions.

```mermaid
flowchart TB
    %% ─────────────────────────── PYTHON ───────────────────────────
    subgraph PY["PYTHON"]
        direction LR
        DRV["main driver<br/>subscribe → backfill → resume<br/>→ register pools + paths"]
        CONSUME["consume_result_batches<br/>async for batch in engine"]
    end

    %% ───────────────────────── PyO3 / FFI ─────────────────────────
    subgraph FFI["PyO3 / FFI — degenbot_rs"]
        PYENG{{"PyUniswapArbEngine<br/>wraps Arc Mutex UniswapEngine<br/>+ async iterator __anext__"}}
    end

    %% ─────────────────────────── RUST ────────────────────────────
    subgraph RS["RUST — UniswapEngine"]
        direction TB

        subgraph S["① STATE HELD"]
            UE["UniswapEngine<br/>• V2 / V3 / V4 block engines<br/>  (reserves, sqrt-price, tick maps)<br/>• path registry + dirty-pool sets<br/>• results + delivered diffs<br/>• result_tx sender"]
        end

        subgraph A["② STATE UPDATED VIA ALLOY"]
            PUMP["UniswapEnginePump<br/>Alloy WS: newHeads + logs<br/>→ decode → apply_log (eager)<br/>→ mark dirty"]
            BFILL["backfill_from_snapshot<br/>Alloy HTTP eth_getLogs → apply"]
            PUMP -->|"mutate"| UE
            BFILL -->|"mutate"| UE
        end

        subgraph M["③ SOLVED WITH MOBIUS"]
            SOLVE["solve_dirty<br/>(coalesced per log burst)"]
            MOB["Mobius optimizer core<br/>mobius_* + IntHopState (U512)<br/>+ bounded LRU pool cache"]
            UE -->|"read dirty paths"| SOLVE
            SOLVE --> MOB
            MOB -->|"write results"| UE
        end

        subgraph D["④ DELIVERED TO PYTHON"]
            DIFF["compute_diff_and_send<br/>(results vs delivered)"]
            CH["unbounded mpsc result_tx"]
            BATCH["ResultBatch<br/>fresh / updated / expired / removed"]
            UE -->|"results vs delivered"| DIFF
            DIFF --> CH --> BATCH
        end
    end

    DRV -->|"register / subscribe / resume"| PYENG
    PYENG -->|"holds lock, delegates"| UE
    BATCH -->|"__anext__ async poll"| PYENG
    PYENG -->|"yield batch"| CONSUME
```

## The four concerns

| # | Concern | Where it lives | Key types |
|---|---------|----------------|-----------|
| ① | **State held** | `UniswapEngine` (Rust), guarded by `parking_lot::Mutex` | `V2/V3/V4BlockEngine`, `path_pools`, `dirty_v2/v3/v4`, `results`, `delivered`, `result_tx` |
| ② | **State updated via Alloy** | `UniswapEnginePump` (WS) + `backfill_from_snapshot` (HTTP) | `AlloyProvider`, `apply_log`, `process_backfill_logs` |
| ③ | **Solved with Mobius** | `solve_dirty` → Mobius math core | `mobius_*`, `IntHopState` (U512), `PyPoolCache` LRU |
| ④ | **Delivered to Python** | mpsc channel → async iterator | `compute_diff_and_send`, `ResultBatch`, `__anext__` |

## Steady-state data flow

```
Alloy WS logs ─► apply_log ─► mark pool dirty ─► solve_dirty ─► Mobius ─► results
                                                                         │
                           compute_diff_and_send ◄────────────────────────┘
                                         │
                          ResultBatch (fresh/updated/expired/removed)
                                         │  unbounded mpsc
                                         ▼
                          PyUniswapArbEngine.__anext__  ─►  Python consumer
```

`UniswapEngine` is the hub: the Alloy pump/backfill **write** to it, the Mobius
solver **reads** dirty paths and **writes** results, and `compute_diff_and_send`
**reads** `results` vs `delivered` to produce the incremental batch that crosses
the FFI boundary back to Python.
