"""
Command-stream parser and verification assertions for three-hop tests.

Parses the binary command stream emitted by the encoding helpers in
conftest_shared.py and counts operations by type. Used to assert exact
transfer counts, V4 operation counts, and profit amounts in test methods.

Command format (after the 0xFF execution separator):
  Fixed-size commands: [opcode:1][fixed fields]
  Variable-size commands: [opcode:1][len:2][payload:len]

ERC20 transfer count = count of 0x10 (ERC20_TRANSFER) opcodes.
This is the definitive "transfer count" used in the docstring claims.
"""

# ── Command opcodes (must match contracts/cmd_executor.vy) ──

CMD_ERC20_TRANSFER = 0x10
CMD_ERC20_XFER_BALANCE = 0x11
CMD_WETH_DEPOSIT = 0x12
CMD_WETH_WITHDRAW = 0x13
CMD_WETH_DEPOSIT_ALL = 0x14
CMD_WETH_WITHDRAW_ALL = 0x15
CMD_SEND_ETH = 0x16
CMD_SEND_ETH_ALL = 0x17

CMD_V2_SWAP_COMPACT = 0x20
CMD_V2_SWAP_CALC = 0x21
CMD_V2_SWAP_DIRECT = 0x22

CMD_V3_SWAP_COMPACT = 0x30
CMD_V3_SWAP_DELTA = 0x31

CMD_V4_SWAP_COMPACT = 0x40
CMD_V4_SWAP_DYNAMIC = 0x41
CMD_V4_BATCH = 0x42

CMD_V4_UNLOCK = 0x50
CMD_V4_TAKE = 0x51
CMD_V4_TAKE_COMPACT = 0x52
CMD_V4_TAKE_DELTA = 0x53
CMD_V4_SYNC = 0x54
CMD_V4_SETTLE = 0x55
CMD_V4_SETTLE_DELTA = 0x56
CMD_V4_SETTLE_ALL = 0x57
CMD_V4_MINT_COMPACT = 0x58
CMD_V4_BURN_COMPACT = 0x59

# Preprocessing opcodes (not counted)
CMD_SET_ADDRESS = 0x00
CMD_SET_EXPECTED_BALANCE = 0x01
CMD_BRIBE_COINBASE = 0x02
CMD_BRIBE_ADDRESS = 0x03

BEGIN_EXECUTION = 0xFF

# ── Fixed command sizes (must match contracts/cmd_executor.vy) ──

FIXED_SIZES = {
    CMD_V4_SETTLE: 1,
    CMD_V4_SETTLE_ALL: 1,
    CMD_WETH_DEPOSIT_ALL: 1,
    CMD_WETH_WITHDRAW_ALL: 1,
    CMD_SEND_ETH_ALL: 2,
    CMD_V4_SYNC: 2,
    CMD_V4_SETTLE_DELTA: 2,
    CMD_ERC20_XFER_BALANCE: 3,
    CMD_V4_TAKE_DELTA: 3,
    CMD_V2_SWAP_CALC: 6,
    CMD_V2_SWAP_DIRECT: 20,
    CMD_V3_SWAP_DELTA: 4,
    CMD_V4_SWAP_DYNAMIC: 11,
    CMD_SEND_ETH: 18,
    CMD_V4_BURN_COMPACT: 18,
    CMD_V4_TAKE_COMPACT: 19,
    CMD_V4_MINT_COMPACT: 19,
    CMD_V4_SWAP_COMPACT: 27,
    CMD_WETH_DEPOSIT: 33,
    CMD_WETH_WITHDRAW: 33,
    CMD_ERC20_TRANSFER: 35,
    CMD_V4_TAKE: 35,
}

# Variable-size: use [opcode:1][len:2] header, size = 3 + len
VARIABLE_SIZE_OPCODES = {
    CMD_V4_UNLOCK,
    CMD_V4_BATCH,
}

# Late-length commands: the forward_len field appears deep in the command,
# not right after the opcode. Each needs custom offset logic.
LATE_LENGTH_OPCODES = {
    CMD_V2_SWAP_COMPACT,
    CMD_V3_SWAP_COMPACT,
}

# ── Counting categories ──

ERC20_TRANSFER_OPCODES = {CMD_ERC20_TRANSFER}
V4_TAKE_OPCODES = {CMD_V4_TAKE, CMD_V4_TAKE_COMPACT, CMD_V4_TAKE_DELTA}
V4_MINT_OPCODES = {CMD_V4_MINT_COMPACT}
V4_BURN_OPCODES = {CMD_V4_BURN_COMPACT}
V4_SETTLE_OPCODES = {CMD_V4_SETTLE, CMD_V4_SETTLE_DELTA, CMD_V4_SETTLE_ALL}
V4_SWAP_OPCODES = {CMD_V4_SWAP_COMPACT, CMD_V4_SWAP_DYNAMIC}
V2_SWAP_OPCODES = {CMD_V2_SWAP_COMPACT, CMD_V2_SWAP_CALC, CMD_V2_SWAP_DIRECT}
V3_SWAP_OPCODES = {CMD_V3_SWAP_COMPACT, CMD_V3_SWAP_DELTA}

# The "official" transfer count = ERC20_TRANSFER + V4_TAKE + V2_SWAP + V3_SWAP.
# Every V2/V3 swap sends tokens to a recipient — that's an ERC20 transfer
# performed by the pair contract. V4_TAKE also transfers ERC20 out of PM.
# V4_SWAP does NOT transfer (delta accounting only inside PM).
# V4_MINT/BURN are internal accounting, not counted as transfers.
ALL_TRANSFER_OPCODES = ERC20_TRANSFER_OPCODES | V4_TAKE_OPCODES | V2_SWAP_OPCODES | V3_SWAP_OPCODES


def parse_commands(data: bytes, recurse_unlock: bool = True) -> list[dict]:
    """Parse the execution section of a command stream into a list of commands.

    `data` should be the FULL stream including preamble. The parser skips
    the preprocessing section (0xFE + SET_ADDRESS commands + SKIP_PROFIT +
    0xFF separator) and then parses execution commands.

    If recurse_unlock is True, V4_UNLOCK payloads are parsed recursively
    so that V4_TAKE/V4_SWAP/etc. inside unlock() are counted.

    Returns a list of dicts: {"opcode": int, "offset": int, "size": int,
                              "inner": list[dict] | None (for V4_UNLOCK)}
    """
    # Parse preprocessing section to find the execution start
    if len(data) == 0 or data[0] != 0xFE:
        # No preamble — treat entire stream as execution
        exec_start = 0
    else:
        # Skip preamble: [0xFE][SET_ADDRESS 21b each][0x01][0xFF]
        offset = 1  # skip 0xFE
        while offset < len(data) and data[offset] == CMD_SET_ADDRESS:
            offset += 21  # [0x00][address:20]
        # Skip optional bribe commands (0x02, 0x03)
        while offset < len(data) and data[offset] in (0x02, 0x03):
            if data[offset] == 0x02:
                offset += 3  # [0x02][bips:2]
            elif data[offset] == 0x03:
                offset += 4  # [0x03][recipient:1][bips:2]
        # Skip SET_EXPECTED_BALANCE (0x01)[balance:12]
        if offset < len(data) and data[offset] == CMD_SET_EXPECTED_BALANCE:
            offset += 13  # [0x01][expected_balance:12]
        # The next byte should be 0xFF (BEGIN_EXECUTION)
        if offset < len(data) and data[offset] == BEGIN_EXECUTION:
            exec_start = offset + 1
        else:
            raise ValueError(
                f"Expected 0xFF execution separator at offset {offset}, "
                f"got 0x{data[offset]:02x}" if offset < len(data) else "EOF"
            )

    commands = _parse_command_list(data, exec_start, len(data), recurse_unlock)
    return commands


def _parse_command_list(data: bytes, start: int, end: int, recurse_unlock: bool) -> list[dict]:
    """Parse commands from data[start:end]."""
    offset = start
    commands = []

    while offset < end:
        opcode = data[offset]

        if opcode in VARIABLE_SIZE_OPCODES:
            # [opcode:1][len:2][payload:len]
            if offset + 3 > end:
                break
            length = int.from_bytes(data[offset + 1: offset + 3], "big")
            cmd_size = 3 + length
        elif opcode in LATE_LENGTH_OPCODES:
            # V2/V3 swap compact: forward_len field appears at a fixed offset
            # past the opcode, not right after it.
            # V2_SWAP_COMPACT: [0x20][pool:1][zfo:1][amount:16][recipient:1][fee:2][fwd_len:2][fwd:N]
            #   fwd_len at offset+22, header = 24 bytes before forward_data
            # V3_SWAP_COMPACT: [0x30][pool:1][zfo:1][amount:16][recipient:1][fwd_len:2][fwd:N]
            #   fwd_len at offset+20, header = 22 bytes before forward_data
            if opcode == CMD_V2_SWAP_COMPACT:
                fwd_len_off = offset + 22
                header_size = 24
            else:  # CMD_V3_SWAP_COMPACT
                fwd_len_off = offset + 20
                header_size = 22
            if fwd_len_off + 2 > end:
                break
            fwd_len = int.from_bytes(data[fwd_len_off:fwd_len_off + 2], "big")
            cmd_size = header_size + fwd_len
        elif opcode in FIXED_SIZES:
            cmd_size = FIXED_SIZES[opcode]
        else:
            raise ValueError(
                f"Unknown opcode 0x{opcode:02x} at offset {offset}"
            )

        entry = {"opcode": opcode, "offset": offset, "size": cmd_size, "inner": None}

        # Recurse into V4_UNLOCK payloads
        if recurse_unlock and opcode == CMD_V4_UNLOCK:
            payload_start = offset + 3  # skip [0x50][len:2]
            payload_end = offset + cmd_size
            entry["inner"] = _parse_command_list(
                data, payload_start, payload_end, recurse_unlock
            )

        # Recurse into V2/V3 SWAP_COMPACT forward_data
        # V2_SWAP_COMPACT: [0x20][pool:1][zfo:1][amount:16][recipient:1][fee:2][fwd_len:2][fwd:N]
        #   forward_data starts at offset+24, length at offset+22..24
        # V3_SWAP_COMPACT: [0x30][pool:1][zfo:1][amount:16][recipient:1][fwd_len:2][fwd:N]
        #   forward_data starts at offset+22, length at offset+20..22
        if recurse_unlock and opcode in (CMD_V2_SWAP_COMPACT, CMD_V3_SWAP_COMPACT):
            if opcode == CMD_V2_SWAP_COMPACT:
                fwd_len_offset = offset + 22  # past the 2-byte fee field
            else:
                fwd_len_offset = offset + 20
            if fwd_len_offset + 2 <= offset + cmd_size:
                fwd_len = int.from_bytes(data[fwd_len_offset:fwd_len_offset + 2], "big")
                fwd_start = fwd_len_offset + 2
                fwd_end = fwd_start + fwd_len
                if fwd_end <= offset + cmd_size and fwd_len > 0:
                    entry["inner"] = _parse_command_list(
                        data, fwd_start, fwd_end, recurse_unlock
                    )

        commands.append(entry)
        offset += cmd_size

    return commands


def _collect_opcodes(commands: list[dict], opcode_set: set[int]) -> int:
    """Count commands matching any opcode in opcode_set, recursing into V4_UNLOCK."""
    total = 0
    for c in commands:
        if c["opcode"] in opcode_set:
            total += 1
        if c["inner"] is not None:
            total += _collect_opcodes(c["inner"], opcode_set)
    return total


def _collect_all(commands: list[dict]) -> list[dict]:
    """Flatten commands including V4_UNLOCK inner payloads."""
    result = []
    for c in commands:
        result.append(c)
        if c["inner"] is not None:
            result.extend(_collect_all(c["inner"]))
    return result


def count_ops(commands: list[dict], opcode_set: set[int]) -> int:
    """Count commands matching any opcode in opcode_set, recursing into V4_UNLOCK."""
    return _collect_opcodes(commands, opcode_set)


def count_transfers(commands: list[dict]) -> int:
    """Count ERC20 transfers: ERC20_TRANSFER + V4_TAKE + V2_SWAP + V3_SWAP.

    This is the "official" transfer count used in all docstrings and docs.
    - ERC20_TRANSFER (0x10): explicit token transfer
    - V4_TAKE (0x51/0x52/0x53): PM sends tokens out (ERC20 transfer)
    - V2_SWAP (0x20/0x21): V2 pair sends output tokens (ERC20 transfer)
    - V3_SWAP (0x30/0x31): V3 pool sends output tokens (ERC20 transfer)

    V4_MINT and V4_BURN are NOT counted (internal PM accounting).
    V4_SWAP is NOT counted (delta accounting only inside PM).
    """
    return count_ops(commands, ALL_TRANSFER_OPCODES)


def summarize_commands(commands: list[dict]) -> dict[str, int]:
    """Return a breakdown of all command categories."""
    return {
        "erc20_transfer": count_ops(commands, ERC20_TRANSFER_OPCODES),
        "v4_take": count_ops(commands, V4_TAKE_OPCODES),
        "v4_mint": count_ops(commands, V4_MINT_OPCODES),
        "v4_burn": count_ops(commands, V4_BURN_OPCODES),
        "v4_settle": count_ops(commands, V4_SETTLE_OPCODES),
        "v4_swap": count_ops(commands, V4_SWAP_OPCODES),
        "v2_swap": count_ops(commands, V2_SWAP_OPCODES),
        "v3_swap": count_ops(commands, V3_SWAP_OPCODES),
        "v4_unlock": count_ops(commands, {CMD_V4_UNLOCK}),
        "v4_sync": count_ops(commands, {CMD_V4_SYNC}),
        "weth_deposit": count_ops(commands, {CMD_WETH_DEPOSIT, CMD_WETH_DEPOSIT_ALL}),
        "weth_withdraw": count_ops(commands, {CMD_WETH_WITHDRAW, CMD_WETH_WITHDRAW_ALL}),
        "total_transfers": count_transfers(commands),
        "total_commands": len(_collect_all(commands)),
    }


def parse_stream(data: bytes) -> dict:
    """Parse a full command stream and return summary + raw commands."""
    cmds = parse_commands(data)
    summary = summarize_commands(cmds)
    summary["commands"] = cmds
    return summary
