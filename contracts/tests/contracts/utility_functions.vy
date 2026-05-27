@internal
@pure
def _convert_address_to_checksummed_addr_str(input_address: address) -> String[42]:
    """
    @notice Converts an Ethereum address to its ERC-55 mixed-case checksum representation
    @dev Implements ERC-55 mixed-case checksum address encoding (https://eips.ethereum.org/EIPS/eip-55)
         
         The input address is split into two uint256 values to handle the full 20 bytes:
         - first_half: bytes 0-9 
         - second_half: bytes 10-19
         
         For each nibble (4 bits) in the address:
         1. Convert to ASCII hex character using bitwise operations:
            - For 0-9: Add 48 to get ASCII values 48-57 (per RFC 20)
            - For a-f: Add 87 to get ASCII values 97-102
         2. Calculate Keccak-256 hash of the lowercase hex string
         3. For each hex character:
            - Numbers 0-9 remain unchanged
            - Letters a-f are made uppercase if corresponding hash nibble >= 8
            - Uppercase is done by clearing bit 5 (AND with 0xDF)
            - Lowercase is preserved by keeping bit 5 set
         
         ASCII Bit Operations (RFC 20 - https://datatracker.ietf.org/doc/html/rfc20):
         - Decimal digits: 0x30-0x39 (bit pattern: 0011 xxxx)
         - Lowercase a-f: 0x61-0x66 (bit pattern: 0110 xxxx) 
         - Uppercase A-F: 0x41-0x46 (bit pattern: 0100 xxxx)
         - Toggle case: XOR with 0x20 (toggles bit 5)
         
    @param input_address The 20-byte Ethereum address to convert
    @return The ERC-55 mixed-case checksum address as a 42-char string (0x + 40 hex chars)
    """
    addr_int: uint256 = convert(input_address, uint256)
    
    # Split into two 10-byte halves from the right-aligned address
    first_half: uint256 = (addr_int >> 80) & ((1 << 80) - 1)
    second_half: uint256 = addr_int & ((1 << 80) - 1)
    
    first_ascii: uint256 = empty(uint256)
    second_ascii: uint256 = empty(uint256)
    nibble: uint256 = empty(uint256)
    ascii_val: uint256 = empty(uint256)
    
    # To lowercase char array
    for i: uint256 in range(20):
        # Process first half - build from left to right
        nibble = (first_half >> unsafe_sub(76, unsafe_mul(i, 4))) & 15
        ascii_val = unsafe_add(unsafe_add(nibble, 48), unsafe_mul(39, ((unsafe_add(nibble, 6)) >> 4)))
        first_ascii = first_ascii | (ascii_val << unsafe_mul(unsafe_sub(19, i), 8))
        
        # Process second half - build from left to right
        nibble = (second_half >> unsafe_sub(76, unsafe_mul(i, 4))) & 15
        ascii_val = unsafe_add(unsafe_add(nibble, 48), unsafe_mul(39, ((unsafe_add(nibble, 6)) >> 4)))
        second_ascii = second_ascii | (ascii_val << unsafe_mul(unsafe_sub(19, i), 8))
    
    address_hash_int: uint256 = convert(keccak256(concat(
        slice(convert(first_ascii, bytes32), 12, 20),
        slice(convert(second_ascii, bytes32), 12, 20)
    )), uint256)
    
    first_checksum: uint256 = empty(uint256)
    second_checksum: uint256 = empty(uint256)

    # Applying checksum
    for i: uint256 in range(20):
        hash_shift: uint256 = unsafe_sub(252, i << 2)  # Start from most significant nibble
        hash_nibble: uint256 = (address_hash_int >> hash_shift) & 15
            
        char: uint256 = (first_ascii >> (unsafe_sub(19, i) << 3)) & 255
        if char >= 97 and char <= 102 and hash_nibble >= 8:
            char = char & 223  # Convert to uppercase
        first_checksum = first_checksum | (char << (unsafe_sub(19, i) << 3))
        
        hash_shift = unsafe_sub(252, unsafe_add(i, 20) << 2)  # Continue for second half
        hash_nibble = (address_hash_int >> hash_shift) & 15
            
        char = (second_ascii >> (unsafe_sub(19, i) << 3)) & 255
        if char >= 97 and char <= 102 and hash_nibble >= 8:
            char = char & 223  # Convert to uppercase
        second_checksum = second_checksum | (char << (unsafe_sub(19, i) << 3))
    
    return convert(concat(b"0x", slice(convert(first_checksum, bytes32), 12, 20),slice(convert(second_checksum, bytes32), 12, 20)), String[42])