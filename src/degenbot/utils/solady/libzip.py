from eth_typing import HexStr
from hexbytes import HexBytes

MAX_LITERALS = 32
MAX_MATCH_LENGTH = 262
MAX_MATCH_OFFSET = 8191

MAX_1BYTE_INT = 0xFF
MAX_3BYTE_INT = 0xFFFFFF


def flz_compress(uncompressed_data: str | bytes) -> HexBytes:
    """
    Compress data using Solady's FastLZ algorithm.

    ref: https://github.com/Vectorized/solady/blob/main/js/solady.js
    """

    def get_3_byte_int(pos: int) -> int:
        """
        Get a 24-bit (3 byte) unsigned integer from the input buffer at position `pos`.

        The number is constructed from a little-endian memory arrangement (the least significant
        digit appears first).
        """

        return input_buffer[pos] + (input_buffer[pos + 1] << 8) + (input_buffer[pos + 2] << 16)

    def hash_(x: int) -> int:
        """
        Hash the 1-byte value
        """

        return ((2654435769 * x) >> 19) & (MAX_MATCH_OFFSET)

    def add_literals(run_length: int, input_buffer_start_offset: int) -> None:
        """
        Extend the output buffer with a continuous run of 1-byte literals copied from the input
        buffer.
        """

        while run_length > 0:
            number_of_literals = min(run_length, MAX_LITERALS)

            # Encode the run length
            output_buffer.append(number_of_literals - 1)

            # Encode the literals
            output_buffer.extend(
                input_buffer[
                    input_buffer_start_offset : input_buffer_start_offset + number_of_literals
                ]
            )

            run_length -= number_of_literals
            input_buffer_start_offset += number_of_literals

    input_buffer = bytearray(HexBytes(uncompressed_data))
    compressible_bytes = len(input_buffer) - 4
    hash_table: dict[int, int] = {}
    output_buffer = bytearray()

    last_literal_offset = 0
    input_buffer_offset = 2

    while input_buffer_offset < compressible_bytes - 9:
        while True:
            input_chunk = get_3_byte_int(input_buffer_offset)
            input_chunk_hash = hash_(input_chunk)

            last_seen_offset = hash_table.get(input_chunk_hash, 0)

            # Always update the last-seen offset for the chunk
            hash_table[input_chunk_hash] = input_buffer_offset

            # Measure the distance from the last-seen chunk, or the start of the buffer
            # (if this is the first occurence)
            distance_to_last_seen_chunk = input_buffer_offset - last_seen_offset
            reference_chunk = (
                get_3_byte_int(last_seen_offset)
                if distance_to_last_seen_chunk <= MAX_MATCH_OFFSET
                else MAX_3BYTE_INT + 1
            )

            if input_buffer_offset < compressible_bytes - 9 and input_chunk != reference_chunk:
                input_buffer_offset += 1
                continue

            input_buffer_offset += 1
            break

        if input_buffer_offset >= compressible_bytes - 9:
            break

        input_buffer_offset -= 1
        if input_buffer_offset > last_literal_offset:
            add_literals(
                run_length=input_buffer_offset - last_literal_offset,
                input_buffer_start_offset=last_literal_offset,
            )

        p = last_seen_offset + 3  # Start of previous occurrence
        q = input_buffer_offset + 3  # Start of current occurrence
        max_possible_match_length = compressible_bytes - q
        match_length = max_possible_match_length

        # Find maximum match length
        for offset in range(max_possible_match_length):
            if input_buffer[p + offset] != input_buffer[q + offset]:
                match_length = offset + 1
                break

        input_buffer_offset += match_length
        distance_to_last_seen_chunk -= 1  # Adjust offset for encoding

        while match_length >= MAX_MATCH_LENGTH:
            # LONG MATCH instruction - 3 byte opcode
            opcode_0 = 0b11100000 + (distance_to_last_seen_chunk >> 8)
            opcode_1 = MAX_MATCH_LENGTH - 9
            opcode_2 = distance_to_last_seen_chunk & 0b0000011111111

            output_buffer.append(opcode_0)
            output_buffer.append(opcode_1)
            output_buffer.append(opcode_2)

            match_length -= MAX_MATCH_LENGTH

        # Encode the remaining chunk
        if match_length < 7:  # noqa: PLR2004
            # SHORT MATCH instruction - 2 byte opcode
            opcode_0 = (match_length << 5) + (distance_to_last_seen_chunk >> 8)
            opcode_1 = distance_to_last_seen_chunk & 0b11111111

            output_buffer.append(opcode_0)
            output_buffer.append(opcode_1)
        else:
            # LONG MATCH instruction - 3 byte opcode
            opcode_0 = 0b11100000 + (distance_to_last_seen_chunk >> 8)
            opcode_1 = match_length - 7
            opcode_2 = distance_to_last_seen_chunk & 0b0000011111111

            output_buffer.append(opcode_0)
            output_buffer.append(opcode_1)
            output_buffer.append(opcode_2)

        # Update hash table with next positions
        hash_table[hash_(get_3_byte_int(input_buffer_offset))] = input_buffer_offset
        input_buffer_offset += 1
        hash_table[hash_(get_3_byte_int(input_buffer_offset))] = input_buffer_offset
        input_buffer_offset += 1

        # Update last literals position
        last_literal_offset = input_buffer_offset

    # Add remaining literals
    add_literals(
        run_length=compressible_bytes + 4 - last_literal_offset,
        input_buffer_start_offset=last_literal_offset,
    )

    return HexBytes(output_buffer)


def flz_decompress(compressed_data: bytes | bytearray | HexStr) -> HexBytes:
    """
    Decompress data using Solady's FastLZ algorithm.

    ref: https://github.com/Vectorized/solady/blob/main/js/solady.js
    """

    input_buffer = bytearray(HexBytes(compressed_data))
    output_buffer = bytearray()

    while input_buffer:
        opcode_0 = input_buffer.pop(0)
        instruction_type = opcode_0 >> 5

        match instruction_type:
            case 0:  # Literal run
                number_of_literals = 1 + (opcode_0 & 0b00011111)
                for _ in range(number_of_literals):
                    output_buffer.append(input_buffer.pop(0))

            case 1 | 2 | 3 | 4 | 5 | 6:  # Short match
                match_length = 2 + (opcode_0 >> 5)

                opcode_1 = input_buffer.pop(0)
                reference_offset = 256 * (opcode_0 & 0b00011111) + opcode_1

                for _ in range(match_length):
                    output_buffer.append(output_buffer[-1 - reference_offset])

            case 7:  # Long match
                opcode_1 = input_buffer.pop(0)
                match_length = 9 + opcode_1

                opcode_2 = input_buffer.pop(0)
                reference_offset = 256 * (opcode_0 & 0b00011111) + opcode_2

                for _ in range(match_length):
                    output_buffer.append(output_buffer[-1 - reference_offset])

            case _:
                error_message = "Invalid instruction!"
                raise ValueError(error_message)

    return HexBytes(output_buffer)
