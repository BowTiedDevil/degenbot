"""Retry-aware log fetching with adaptive chunk sizing."""

from collections.abc import Sequence

import tqdm
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from requests.exceptions import RequestException
from tenacity import (
    AsyncRetrying,
    RetryError,
    Retrying,
    retry_if_exception_type,
    stop_after_attempt,
    wait_exponential_jitter,
)
from web3._utils.threads import Timeout  # noqa: PLC2701
from web3.exceptions import Web3Exception
from web3.types import LogReceipt

from degenbot.exceptions.infrastructure import LogFetchingTimeout
from degenbot.logging import logger
from degenbot.provider import ProviderAdapter
from degenbot.provider.interface import AsyncProviderAdapter
from degenbot.types.aliases import BlockNumber


def _build_topics_list(
    topic_signature: Sequence[Sequence[HexBytes] | HexBytes] | None,
) -> list[list[str]] | None:
    """Convert topic signature to the list format expected by the RPC call."""
    if not topic_signature:
        return None
    return [
        [t.to_0x_hex() if isinstance(t, HexBytes) else str(t) for t in topic]
        for topic in topic_signature
    ]


class _ChunkedLogFetcher:
    """Internal: shared chunking and span-management logic for log fetching."""

    _DEFAULT_MAX_BLOCKS = 5_000
    _SPAN_REDUCTION_PCT = 25
    _SPAN_INCREASE_PCT = 5

    def __init__(
        self,
        start_block: int,
        end_block: int,
        max_blocks_per_request: int | None,
    ) -> None:
        self.start_block = start_block
        self.end_block = end_block
        self.max_blocks: int = (
            max_blocks_per_request
            if max_blocks_per_request is not None
            else self._DEFAULT_MAX_BLOCKS
        )
        self.working_span = self.max_blocks

    @property
    def chunk_end(self) -> int:
        return min(self.end_block, self.start_block + self.working_span - 1)

    @property
    def is_complete(self) -> bool:
        return self.start_block > self.end_block

    @property
    def chunk_size(self) -> int:
        return self.chunk_end - self.start_block + 1

    def on_timeout(self) -> None:
        self.working_span = max(
            1,
            int(self.working_span - self.working_span * self._SPAN_REDUCTION_PCT / 100),
        )

    def on_success(self) -> None:
        self.working_span = min(
            self.max_blocks,
            int(self.working_span + self.working_span * self._SPAN_INCREASE_PCT / 100),
        )

    def advance(self) -> None:
        self.start_block = self.chunk_end + 1


def fetch_logs_retrying(
    *,
    provider: ProviderAdapter,
    start_block: BlockNumber,
    end_block: BlockNumber,
    max_retries: int = 10,
    max_blocks_per_request: int | None = None,
    address: list[ChecksumAddress] | None = None,
    topic_signature: Sequence[Sequence[HexBytes] | HexBytes] | None = None,
) -> list[LogReceipt]:
    """Fetch all event logs for the given topic signature (or all logs, if omitted), inclusive for the.

    given block range.

    Max blocks per request is set to 5,000 if not specified.

    See `https://ethereum.org/developers/docs/apis/json-rpc/#eth_getfilterchanges` for formatting of
    topic signatures.

    Args:
        provider: ProviderAdapter instance
        start_block: Starting block number
        end_block: Ending block number
        max_retries: Maximum retry attempts
        max_blocks_per_request: Maximum blocks per request
        address: Contract addresses to filter
        topic_signature: Event topic signatures

    """
    if end_block < start_block:
        msg = "End block cannot be earlier than start block."
        raise ValueError(msg)

    if address is None:
        address = []

    if topic_signature is None:
        topic_signature = []

    fetcher = _ChunkedLogFetcher(
        start_block=start_block,
        end_block=end_block,
        max_blocks_per_request=max_blocks_per_request,
    )

    retrier = Retrying(
        stop=stop_after_attempt(max_retries),
        wait=wait_exponential_jitter(),
        retry=retry_if_exception_type(
            (Timeout, Web3Exception, RequestException),
        ),
    )

    event_logs: list[LogReceipt] = []

    def _get_logs(
        from_block: int,
        to_block: int,
        addr: list[ChecksumAddress],
        topics: Sequence[Sequence[HexBytes] | HexBytes],
    ) -> list[LogReceipt]:
        topics_str: list[list[str]] = []
        for topic in topics:
            if isinstance(topic, HexBytes):
                topics_str.append([topic.to_0x_hex()])
            else:
                topics_str.append([t.to_0x_hex() for t in topic])
        return provider.get_logs(
            from_block=from_block,
            to_block=to_block,
            addresses=addr or None,  # ty:ignore[invalid-argument-type]
            topics=topics_str or None,
        )

    while not fetcher.is_complete:
        try:
            for attempt in retrier:
                chunk_end = fetcher.chunk_end

                with attempt:
                    try:
                        logger.debug(
                            f"Fetching logs for range {fetcher.start_block}-{chunk_end} "
                            f" ({fetcher.chunk_size} blocks)"
                        )
                        event_logs.extend(
                            _get_logs(fetcher.start_block, chunk_end, address, topic_signature)
                        )
                    except Exception:
                        old_working_span = fetcher.working_span
                        fetcher.on_timeout()
                        logger.debug(
                            f"Attempt {attempt.retry_state.attempt_number} timed out "
                            f"fetching {old_working_span} blocks. "
                            f"Reducing to {fetcher.working_span}..."
                        )
                        raise
                    else:
                        fetcher.on_success()

            fetcher.advance()

        except RetryError:
            raise LogFetchingTimeout(max_retries=max_retries) from None

    return event_logs


async def fetch_logs_retrying_async(
    *,
    provider: AsyncProviderAdapter,
    start_block: BlockNumber,
    end_block: BlockNumber,
    max_retries: int = 10,
    max_blocks_per_request: int | None = None,
    address: ChecksumAddress | None = None,
    topic_signature: Sequence[Sequence[HexBytes] | HexBytes] | None = None,
) -> list[LogReceipt]:
    """Async version of fetch_logs_retrying."""
    if topic_signature is None:
        topic_signature = []

    fetcher = _ChunkedLogFetcher(
        start_block=start_block,
        end_block=end_block,
        max_blocks_per_request=max_blocks_per_request,
    )

    event_logs: list[LogReceipt] = []

    retrier = AsyncRetrying(
        stop=stop_after_attempt(max_retries),
        retry=retry_if_exception_type((Timeout, Web3Exception)),
    )

    pbar = tqdm.tqdm(
        event_logs,
        total=end_block - start_block + 1,
        desc="Fetching blocks",
        bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
        leave=False,
    )

    async def _fetch_chunk(attempt: object, chunk_end: int) -> None:
        """Fetch a single chunk of logs within a retry attempt."""
        with attempt:  # ty:ignore[invalid-context-manager]
            try:
                logger.debug(
                    f"Fetching logs for range {fetcher.start_block}-{chunk_end} "
                    f" ({fetcher.chunk_size} blocks)"
                )
                addresses_arg: list[str] | None = [address] if address is not None else None
                topics_list = _build_topics_list(topic_signature)
                logs = await provider.get_logs(
                    from_block=fetcher.start_block,
                    to_block=chunk_end,
                    addresses=addresses_arg,
                    topics=topics_list,
                )
                event_logs.extend(logs)
            except (Timeout, Web3Exception):
                old_working_span = fetcher.working_span
                fetcher.on_timeout()
                logger.debug(
                    f"Attempt {attempt.retry_state.attempt_number} timed out "  # ty:ignore[unresolved-attribute]
                    f"fetching {old_working_span} blocks. "
                    f"Reducing to {fetcher.working_span}..."
                )
                raise
            else:
                pbar.update(fetcher.chunk_size)
                fetcher.on_success()

    while not fetcher.is_complete:
        try:
            async for attempt in retrier:
                await _fetch_chunk(attempt, fetcher.chunk_end)

            fetcher.advance()
            if fetcher.is_complete:
                pbar.close()
                return event_logs

        except RetryError:
            raise LogFetchingTimeout(max_retries=max_retries) from None

    # Explicit return silences lint that cannot prove loop termination.
    return event_logs
