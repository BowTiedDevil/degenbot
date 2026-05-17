# Plan 023: Consolidate CLI Pool Update Functions into Parameterized Processor

**Status: COMPLETE**

## Overview

Extract 12+ near-identical pool update functions (`base_aerodrome_v2_pool_updater`, `base_pancakeswap_v2_pool_updater`, etc.) from `src/degenbot/cli/pool.py` (~1900 lines) into a parameterized `PoolEventProcessor` module. Replace copy-paste with declarative configuration per DEX.

## Files Involved

**Existing:**

- `src/degenbot/cli/pool.py` (~1900 lines) — 12+ updater functions, each ~50-70 lines
- Each DEX has an updater: Aerodrome V2/V3, PancakeSwap V2/V3, SushiSwap V2/V3, SwapBased V2, Uniswap V2/V3/V4, Camelot V2

**New:**

- `src/degenbot/cli/pool_processor.py` — Module with `PoolEventProcessor` class and configuration types
- `src/degenbot/cli/pool_processor/types.py` — Dataclasses for decoders and configurations
- `src/degenbot/cli/pool_processor/decoders.py` — Per-DEX decoder implementations

**Modified:**

- `src/degenbot/cli/pool.py` — Replace 12+ functions with decoder registrations and a unified entry point
- `tests/cli/test_pool_processor.py` — Tests for the new processor

**Tests:**

- `tests/cli/test_pool_processor.py` — Unit tests for decoder configuration and event processing
- Existing pool update integration tests should pass unchanged

## Problem

The `pool.py` file contains 12+ updater functions that follow an identical pattern:

```python
def base_aerodrome_v2_pool_updater(
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
    session: Session,
) -> None:
    """Fetch new Aerodrome V2 pools and add metadata to DB."""
    
    database_type = AerodromeV2PoolTable
    
    new_pool_events = get_events_from_contract(
        provider=provider,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=AERODROME_V2_POOLCREATED_EVENT_HASH,
    )
    
    if new_pool_events:
        for new_pool_event in tqdm.tqdm(new_pool_events, ...):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)
            
            (stable,) = abi_decode(["bool"], new_pool_event["topics"][3])
            
            token0_in_db = _get_or_create_token(session, exchange.chain_id, token0)
            token1_in_db = _get_or_create_token(session, exchange.chain_id, token1)
            
            (pool_address, _) = abi_decode(
                types=["address", "uint256"],
                data=new_pool_event["data"],
            )
            
            (fee,) = raw_call(
                provider=provider,
                address=get_checksum_address(exchange.factory),
                calldata=encode_function_calldata(
                    function_prototype="getFee(address,bool)",
                    function_arguments=[pool_address, stable],
                ),
                return_types=["uint256"],
            )
            
            session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=provider.chain_id,
                    stable=stable,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=fee,
                    fee_token1=fee,
                    fee_denominator=10_000,
                )
            )
```

This pattern is repeated 12+ times with variations:

- **Database model type:** `AerodromeV2PoolTable`, `UniswapV3PoolTable`, etc.
- **Event hash:** Each DEX has its own `PoolCreated` event signature
- **Topic decoding:** V2 uses `token0, token1, stable` (topics 1,2,3), V3 uses `token0, token1, fee` (topics 1,2,3), V4 is completely different
- **Extra RPC calls:** Aerodrome calls `getFee(pool, stable)`, Uniswap V3 stores fee in `topic[3]`, some DEXs call `getTokens` for token ordering
- **Database fields:** V2 has `stable` flag, V3 has `tick_spacing`, V4 has `pool_id` and `pool_manager`

The functions are pass-through orchestrators — they don't add domain logic, they just:

1. Fetch events
2. Decode tokens, fees, flags from topics/data
3. Fetch extra data from chain if needed
4. Persist to DB

Applying the **deletion test**: If you deleted all 12 functions, the complexity of "fetch pool events and persist to database" would not vanish — it would reappear as copy-pasted boilerplate. The module is **shallow**.

## Solution

Extract the common pattern into a parameterized `PoolEventProcessor` with pluggable decoders.

### Core Types

```python
# src/degenbot/cli/pool_processor/types.py

from dataclasses import dataclass
from typing import Callable, TYPE_CHECKING
from fractions import Fraction

if TYPE_CHECKING:
    from collections.abc import Sequence
    from eth_typing import ChecksumAddress
    from web3.types import LogReceipt
    from sqlalchemy.orm import Session
    from degenbot.cli.pool_processor.decoders import PoolEventDecoder


@dataclass(frozen=True)
class DecodedPoolEvent:
    """Unified representation of a pool creation event across all DEXs."""

    tokens: Sequence[ChecksumAddress]  # [token0, token1] (ordered)
    pool_address: ChecksumAddress
    fee: int | None  # None if not applicable or encoded elsewhere
    stable: bool | None  # None if not applicable
    tick_spacing: int | None  # None if not applicable (V3, V4)
    fee_denominator: int | None  # None if not applicable
    extra_data: dict[str, int | str | bool]  # DEX-specific fields


@dataclass(frozen=True)
class RPCCallDescriptor:
    """Description of an RPC call to make after decoding the basic event."""

    # Function signature to encode
    function_prototype: str
    # Arguments (can be references to decoded event fields)
    function_arguments: list[str | int | bool]
    # Return types to decode
    return_types: Sequence[str]
    # Return value processing function
    process_return: Callable[[tuple], int | str | bool]


@dataclass(frozen=True)
class PoolRecordBuilder:
    """Describes how to build the database record from decoded event data."""

    database_type: type  # The SQLAlchemy model class (e.g., AerodromeV2PoolTable)
    field_mapping: dict[str, str]  # Maps record field names -> data keys in DecodedPoolEvent
    constant_fields: dict[str, int | str]  # Fields with constant values (e.g., fee_denominator)


@dataclass(frozen=True)
class PoolEventDecoderConfig:
    """Complete configuration for a DEX's pool update behavior."""

    name: str  # e.g., "aerodrome_v2"
    event_topic_hash: str  # Keccak-256 of "PoolCreated(address,address,uint256,...)"
    pool_creation_event_index: int = 0  # Event index in logs if multiple events per tx
    decode_topics_fn: Callable[[LogReceipt], DecodedPoolEvent]  # Extracts event fields
    rpc_calls: Sequence[RPCCallDescriptor] = ()  # Optional RPC calls to make
    pool_record_builder: PoolRecordBuilder  # How to build the DB record


@dataclass(frozen=True)
class PoolUpdateResult:
    """Result of processing pool update events."""

    pools_added: int
    events_processed: int
    errors: list[tuple[LogReceipt, Exception]]
```

### PoolEventProcessor Interface

```python
# src/degenbot/cli/pool_processor.py

from typing import Any
from tqdm import tqdm
from web3.types import LogReceipt
from sqlalchemy.orm import Session

from degenbot.cli.functions import (
    encode_function_calldata,
    raw_call,
    get_events_from_contract,
    abi_decode,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.provider.interface import ProviderAdapter
from degenbot.database.models.base import ExchangeTable
from degenbot.cli.pool_processor.types import (
    DecodedPoolEvent,
    PoolEventDecoderConfig,
    PoolUpdateResult,
)
from degenbot.erc20 import Erc20Token


class PoolEventProcessor:
    """
    Universal processor for pool update events across all DEXs.

    Replaces 12+ copy-paste functions with a single process() method
    configured per DEX via PoolEventDecoderConfig.
    """

    def __init__(self) -> None:
        self._decoders: dict[str, PoolEventDecoderConfig] = {}
        self._register_default_decoders()

    def register_decoder(
        self,
        config: PoolEventDecoderConfig,
    ) -> None:
        """Register a decoder configuration for a DEX."""
        self._decoders[config.name] = config

    def process(
        self,
        provider: ProviderAdapter,
        start_block: int,
        end_block: int,
        exchange: ExchangeTable,
        session: Session,
        decoder_name: str,
    ) -> PoolUpdateResult:
        """
        Process pool creation events for a DEX using its registered decoder.

        Args:
            provider: RPC provider for the target chain
            start_block: Starting block for event query
            end_block: Ending block for event query
            exchange: Exchange database record with factory address
            session: Database session for persistence
            decoder_name: Name of registered decoder config (e.g., "aerodrome_v2")

        Returns:
            PoolUpdateResult with counts and any errors
        """
        decoder = self._decoders.get(decoder_name)
        if decoder is None:
            raise ValueError(f"Unknown decoder: {decoder_name}")

        # Fetch events from chain
        events = get_events_from_contract(
            provider=provider,
            start_block=start_block,
            end_block=end_block,
            address=get_checksum_address(exchange.factory),
            event_hash=decoder.event_topic_hash,
        )

        result = PoolUpdateResult(
            pools_added=0,
            events_processed=0,
            errors=[],
        )

        if not events:
            return result

        # Process each event
        for event in tqdm.tqdm(
            events,
            desc=f"Adding {decoder.name} pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            result.events_processed += 1

            try:
                # 1. Decode basic event data
                decoded = decoder.decode_topics_fn(event)

                # 2. Fetch tokens from DB or create
                tokens_in_db = [
                    self._get_or_create_token(session, exchange.chain_id, token_address)
                    for token_address in decoded.tokens
                ]

                # 3. Perform any configured RPC calls
                rpc_results: dict[str, Any] = {}
                for rpc_call in decoder.rpc_calls:
                    # Substitute field references in arguments
                    args = self._resolve_arguments(rpc_call.function_arguments, decoded)
                    call_result = raw_call(
                        provider=provider,
                        address=get_checksum_address(exchange.factory),
                        calldata=encode_function_calldata(
                            function_prototype=rpc_call.function_prototype,
                            function_arguments=args,
                        ),
                        return_types=rpc_call.return_types,
                    )
                    rpc_results[rpc_call.process_return.__name__] = rpc_call.process_return(
                        call_result
                    )

                # 4. Build database record
                pool_record = self._build_pool_record(
                    decoder.pool_record_builder,
                    exchange=exchange,
                    chain_id=provider.chain_id,
                    decoded=decoded,
                    tokens_in_db=tokens_in_db,
                    rpc_results=rpc_results,
                )

                # 5. Persist
                session.add(pool_record)
                result.pools_added += 1

            except Exception as exc:
                result.errors.append((event, exc))

        return result

    def _resolve_arguments(
        self,
        arguments: list[str | int | bool],
        decoded: DecodedPoolEvent,
    ) -> list[str | int | bool]:
        """Substitute field references in RPC call arguments."""
        resolved = []
        for arg in arguments:
            if isinstance(arg, str) and arg.startswith("${"):
                # Field substitution: ${pool_address}, ${token0}, etc.
                field_name = arg[2:-1]
                if hasattr(decoded, field_name):
                    resolved.append(getattr(decoded, field_name))
                else:
                    resolved.append(arg)
            else:
                resolved.append(arg)
        return resolved

    def _get_or_create_token(
        self,
        session: Session,
        chain_id: int,
        address: str,
    ) -> Any:
        """Get token from database or create it."""
        from degenbot.database.models.erc20 import Erc20TokenTable
        from sqlalchemy import select

        checksummed = get_checksum_address(address)
        token = session.scalar(
            select(Erc20TokenTable).where(
                Erc20TokenTable.address == checksummed,
                Erc20TokenTable.chain == chain_id,
            ),
        )

        if token is None:
            token = Erc20TokenTable(
                address=checksummed,
                chain=chain_id,
                # name, symbol, decimals are filled later by a separate process
            )
            session.add(token)
            session.flush()

        return token

    def _build_pool_record(
        self,
        builder: Any,
        exchange: ExchangeTable,
        chain_id: int,
        decoded: DecodedPoolEvent,
        tokens_in_db: list[Any],
        rpc_results: dict[str, Any],
    ) -> Any:
        """Build the database record from decoded data."""
        record_kwargs = {
            "exchange_id": exchange.id,
            "address": decoded.pool_address,
            "chain": chain_id,
        }

        # Map field names to decoded data
        for field_name, data_key in builder.field_mapping.items():
            if data_key.startswith("rpc:"):
                rpc_key = data_key[4:]
                if rpc_key in rpc_results:
                    record_kwargs[field_name] = rpc_results[rpc_key]
            elif hasattr(decoded, data_key):
                record_kwargs[field_name] = getattr(decoded, data_key)

        # Constant fields
        for field_name, value in builder.constant_fields.items():
            record_kwargs[field_name] = value

        # Token IDs
        if len(tokens_in_db) >= 2:
            record_kwargs["token0_id"] = tokens_in_db[0].id
            record_kwargs["token1_id"] = tokens_in_db[1].id
        if len(tokens_in_db) >= 3:
            record_kwargs["token2_id"] = tokens_in_db[2].id

        return builder.database_type(**record_kwargs)

    def _register_default_decoders(self) -> None:
        """Register the default set of DEX decoders (called from __init__)."""
        # Import decoders to register themselves
        from degenbot.cli.pool_processor.decoders import (
            register_aerodrome_decoders,
            register_pancakeswap_decoders,
            register_uniswap_decoders,
            register_sushiswap_decoders,
            register_swapbased_decoders,
            register_camelot_decoders,
        )

        register_aerodrome_decoders(self)
        register_pancakeswap_decoders(self)
        register_uniswap_decoders(self)
        register_sushiswap_decoders(self)
        register_swapbased_decoders(self)
        register_camelot_decoders(self)
```

### Decoder Implementations

```python
# src/degenbot/cli/pool_processor/decoders/aerodrome.py

from typing import Callable
from eth_typing import ChecksumAddress
from web3.types import LogReceipt
from fractions import Fraction

from degenbot.cli.pool_processor.types import (
    DecodedPoolEvent,
    PoolEventDecoderConfig,
    RPCCallDescriptor,
    PoolRecordBuilder,
)


def decode_aerodrome_v2(event: LogReceipt) -> DecodedPoolEvent:
    """Decode Aerodrome V2 PoolCreated event topics/data."""
    from degenbot import abi_decode
    from degenbot.checksum_cache import get_checksum_address

    # topics: [signature, token0, token1, stable]
    (token0,) = abi_decode(["address"], event["topics"][1])
    (token1,) = abi_decode(["address"], event["topics"][2])
    (stable,) = abi_decode(["bool"], event["topics"][3])

    # data: [pool_address, ...]
    (pool_address, _) = abi_decode(
        types=["address", "uint256"],
        data=event["data"],
    )

    return DecodedPoolEvent(
        tokens=[get_checksum_address(token0), get_checksum_address(token1)],
        pool_address=get_checksum_address(pool_address),
        fee=None,  # Fetched via RPC
        stable=stable,
        tick_spacing=None,
        fee_denominator=None,
        extra_data={},
    )


def process_aerodrome_fee(call_result: tuple) -> int:
    """Extract fee from getFee RPC call result."""
    return call_result[0]  # getFee returns uint256


def register_aerodrome_v2_decoder(processor: PoolEventProcessor) -> None:
    """Register Aerodrome V2 decoder configuration."""
    from degenbot.cli.pool_processor.types import PoolEventDecoderConfig

    AERODROME_V2_POOLCREATED_EVENT_HASH = (
        "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4d6b"
    )

    processor.register_decoder(
        PoolEventDecoderConfig(
            name="aerodrome_v2",
            event_topic_hash=AERODROME_V2_POOLCREATED_EVENT_HASH,
            decode_topics_fn=decode_aerodrome_v2,
            rpc_calls=[
                RPCCallDescriptor(
                    function_prototype="getFee(address,bool)",
                    function_arguments=["${pool_address}", "${stable}"],
                    return_types=["uint256"],
                    process_return=process_aerodrome_fee,
                ),
            ],
            pool_record_builder=PoolRecordBuilder(
                database_type=AerodromeV2PoolTable,
                field_mapping={
                    "stable": "stable",
                    "fee_token0": "rpc:fee",
                    "fee_token1": "rpc:fee",
                },
                constant_fields={
                    "fee_denominator": 10_000,
                },
            ),
        ),
    )


def register_aerodrome_decoders(processor: PoolEventProcessor) -> None:
    """Register all Aerodrome decoders."""
    register_aerodrome_v2_decoder(processor)
    # register_aerodrome_v3_decoder(processor)  # TODO when V3 is needed
```

### Refactored pool.py Entry Points

```python
# src/degenbot/cli/pool.py (after refactoring)

# Create a single global processor instance
_pool_processor = PoolEventProcessor()


def base_aerodrome_v2_pool_updater(
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
    session: Session,
) -> None:
    """Fetch new Aerodrome V2 pools and add metadata to DB."""
    result = _pool_processor.process(
        provider=provider,
        start_block=start_block,
        end_block=end_block,
        exchange=exchange,
        session=session,
        decoder_name="aerodrome_v2",
    )

    if result.errors:
        logger.error(f"{len(result.errors)} errors processing pools")


def base_pancakeswap_v2_pool_updater(...) -> None:
    """Fetch new PancakeSwap V2 pools and add metadata to DB."""
    result = _pool_processor.process(
        provider=provider,
        start_block=start_block,
        end_block=end_block,
        exchange=exchange,
        session=session,
        decoder_name="pancakeswap_v2",
    )
    if result.errors:
        logger.error(f"{len(result.errors)} errors processing pools")


# Similar thin wrappers for all other DEX updaters...
```

## Implementation Steps

### Phase 1: Create Core Types (TDD)

1. **Red:** Write tests for `DecodedPoolEvent`, `PoolEventDecoderConfig`, `RPCCallDescriptor`, `PoolRecordBuilder`.

   ```python
   def test_decoded_pool_event_immutable():
       event = DecodedPoolEvent(
           tokens=("0x...", "0x..."),
           pool_address="0xPool...",
           fee=3000,
           # ...
       )
       with pytest.raises(dataclasses.FrozenInstanceError):
           event.pool_address = "0xNew..."
   ```

2. **Green:** Create `src/degenbot/cli/pool_processor/types.py` with all frozen dataclasses.

3. Create `src/degenbot/cli/pool_processor/__init__.py`.

### Phase 2: Extract PoolEventProcessor Core Logic (TDD)

1. **Red:** Write test for `_resolve_arguments()` field substitution.
2. **Red:** Write test for `_get_or_create_token()` with mock session.
3. **Red:** Write test for `_build_pool_record()` with mock builder.
4. **Green:** Implement `PoolEventProcessor` class with the three helper methods.
5. Run tests.

### Phase 3: Implement Aerodrome V2 Decoder (TDD)

1. **Red:** Write test for `decode_aerodrome_v2()` with fake event.

   ```python
   def test_decode_aerodrome_v2():
       event = {
           "topics": [
               SIG_HASH,
               "0xA0b8...token0",
               "0x6B17...token1",
               True,  # stable
           ],
           "data": encode(["0xPool...", 123]),
       }
       decoded = decode_aerodrome_v2(event)
       assert decoded.stable is True
       assert len(decoded.tokens) == 2
   ```

2. **Green:** Implement `decode_aerodrome_v2()` in `decoders/aerodrome.py`.
3. Create `src/degenbot/cli/pool_processor/decoders/__init__.py`.
4. Write test for `register_aerodrome_v2_decoder()`.

### Phase 4: Wire Up processor.process() (TDD)

1. **Red:** Write integration test for `process()` with mock provider and session.
2. **Green:** Implement the `process()` method with event fetching and iteration.
3. Verify test passes.

### Phase 5: Replace First Updater Function

1. **Red:** Existing `base_aerodrome_v2_pool_updater` test should pass after refactoring.
2. **Green:** Replace `base_aerodrome_v2_pool_updater` implementation in `pool.py` with thin wrapper to `_pool_processor.process()`.
3. Run existing integration test — should pass unchanged.

### Phase 6: Implement Remaining V2 Decoders

1. For each DEX (PancakeSwap V2, SushiSwap V2, SwapBased V2, Uniswap V2):
   - Write decoder function test
   - Implement decoder
   - Register with processor
   - Replace updater function with thin wrapper
   - Run tests

### Phase 7: Implement V3 Decoders (with fee in topics)

1. Write test for V3 decoder (fee encoded in topic[3] instead of RPC call).
2. Implement `decode_uniswap_v3()` with `fee` extracted from topics.
3. Register and replace functions.

### Phase 8: Implement V4 Decoder (completely different)

1. V4 events don't follow the V2/V3 pattern — study differences.
2. Write decoder function test.
3. Implement decoder and register.

### Phase 9: Verify and Clean Up

1. `just test-all` — all tests pass.
2. `just lint` — no new warnings.
3. Verify `pool.py` line count reduced by ~1000 lines (12 functions × ~80 lines each).
4. Verify all DEX updaters work via integration tests.
5. Check for any missed edge cases (events with extra data, RPC failures, etc.).

## What Stays the Same

- Database models (`AerodromeV2PoolTable`, `UniswapV3PoolTable`, etc.) — unchanged
- CLI command signatures (`degenbot pool update`) — unchanged
- Event fetching mechanism (`get_events_from_contract`) — unchanged
- Token creation logic (`_get_or_create_token`) — moved into processor as a helper
- Integration tests — same behavior, updated imports

## What Changes

| Before | After |
|--------|-------|
| 12+ 50-line updater functions in `pool.py` | 12 one-line thin wrappers |
| Event decoding logic duplicated 12 times | 12 decoder functions, one per DEX |
| RPC call logic embedded in each updater | Declarative `RPCCallDescriptor` configs |
| Database record construction duplicated | `PoolRecordBuilder` configuration |
| Adding new DEX requires copy-pasting function | Add decoder and register |

## Metrics

| Metric | Before | After |
|--------|-------|-------|
| `pool.py` lines | ~1900 | ~900 (12 functions × ~80 lines removed) |
| Updater function count | 12+ | 0 (all become thin wrappers) |
| Decoder modules | 0 | 6 (aerodrome, pancakeswap, uniswap, sushiswap, swapbased, camelot) |
| Lines of decoder code | 0 | ~300 (12 decoders × ~25 lines each) |
| Code duplication | 12 copies of same pattern | Single processor implementation |
| Time to add new DEX | ~80 lines of copy-paste | ~25 lines of decoder function |

## Risks and Mitigations

| Risk | Mitigation |
|--------|------------|
| Decoder field mapping errors lead to wrong DB writes | Write unit tests for each decoder with fake events. Verify `PoolRecordBuilder` maps correctly. |
| RPC call argument substitution fails | Test `_resolve_arguments()` thoroughly with all substitution patterns (`${pool_address}`, `${token0}`, literal values). |
| Decoder registration order matters | Decoders are looked up by name, order doesn't matter. Add validation for duplicate names. |
| V4 events are completely different | Implement V4 decoder last, after V2/V3 pattern is solid. Keep flexibility in `DecodedPoolEvent` for extra fields. |
| Performance regression from abstraction | The processor adds a thin layer, no extra RPC calls. Benchmark before/after — should be identical. |
| Existing integration tests break | Each phase replaces one function at a time. Run tests after each phase. Keep thin wrappers for backward compatibility. |

## Definition of Done

- [ ] `src/degenbot/cli/pool_processor/types.py` created with all types
- [ ] `PoolEventProcessor` core implemented and tested
- [ ] All 12 DEX decoders implemented and tested
- [ ] All updater functions in `pool.py` replaced with thin wrappers
- [ ] All existing integration tests pass
- [ ] `just test-all` passes
- [ ] `just lint` passes
- [ ] `pool.py` line count reduced by ~1000 lines
- [ ] No performance regression (verify with benchmark)
- [ ] Documentation updated with examples of adding new DEX decoders
