import { tool } from "@opencode-ai/plugin"
import { resolveRpcUrl } from "./utils/rpc-resolver"

// @ts-ignore  
const bunRuntime = Bun as any

export const four_byte = tool({
  description: "Get the function signature for the given selector",
  args: {
    selector: tool.schema.string().describe("The function selector"),
  },
  async execute(args) {
    const cmd = ["cast", "4byte"]
    cmd.push(args.selector)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const four_byte_calldata = tool({
  description: "Decode ABI-encoded calldata",
  args: {
    calldata: tool.schema.string().describe("The ABI-encoded calldata"),
  },
  async execute(args) {
    const cmd = ["cast", "4byte-calldata"]
    cmd.push(args.calldata)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const four_byte_event = tool({
  description: "Get the event signature for a given topic0",
  args: {
    topic0: tool.schema.string().describe("Topic 0"),
  },
  async execute(args) {
    const cmd = ["cast", "4byte-event"]
    cmd.push(args.topic0)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const abi_encode = tool({
  description: "ABI encode the given function argument, excluding the selector",
  args: {
    sig: tool.schema.string().describe("The function signature"),
    args: tool.schema.array(tool.schema.string()).describe("The arguments of the function"),
    packed: tool.schema.boolean().optional().describe("Whether to use packed encoding"),
  },
  async execute(args) {
    const cmd = ["cast", "abi-encode"]
    cmd.push(args.sig)
    cmd.push(...args.args)
    if (args.packed) {
      cmd.push("--packed")
    }
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const abi_encode_event = tool({
  description: "ABI encode an event and its arguments to generate topics and data",
  args: {
    sig: tool.schema.string().describe("The event signature"),
    args: tool.schema.array(tool.schema.string()).describe("The arguments of the event"),
  },
  async execute(args) {
    const cmd = ["cast", "abi-encode-event"]
    cmd.push(args.sig)
    cmd.push(...args.args)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const admin = tool({
  description: "Fetch the EIP-1967 admin account",
  args: {
    who: tool.schema.string().describe("The address from which the admin account will be fetched"),
    block: tool.schema.string().optional().describe("The block height to query at"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
  },
  async execute(args, context) {
    const cmd = ["cast", "admin"]
    cmd.push(args.who)
    if (args.block) cmd.push("--block", args.block)
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const age = tool({
  description: "Get the timestamp of a block",
  args: {
    block: tool.schema.string().optional().describe("The block height to query at"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
  },
  async execute(args, context) {
    const cmd = ["cast", "age"]
    if (args.block) cmd.push(args.block)
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const balance = tool({
  description: "Get the balance of an account in wei",
  args: {
    who: tool.schema.string().describe("The account to get the balance for"),
    block: tool.schema.string().optional().describe("The block height to query at. Can also be the tags earliest, finalized, safe, latest, or pending."),
    ether: tool.schema.boolean().optional().describe("Convert the balance to ether"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
    erc20: tool.schema.string().optional().describe("The ERC20 token address"),
  },
  async execute(args, context) {
    const cmd = ["cast", "balance"]
    if (args.block) {
      cmd.push("--block", args.block)
    }
    if (args.ether) {
      cmd.push("--ether")
    }
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    if (args.erc20) {
      cmd.push("--erc20", args.erc20)
    }
    cmd.push(args.who)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const block = tool({
  description: "Get information about a block",
  args: {
    block: tool.schema.string().optional().describe("The block height to query at. Can also be the tags earliest, finalized, safe, latest, or pending."),
    raw: tool.schema.boolean().optional().describe("Print the block header as raw RLP bytes"),
    full: tool.schema.boolean().optional().describe("Print the full block information"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
  },
  async execute(args, context) {
    const cmd = ["cast", "block"]
    if (args.raw) {
      cmd.push("--raw")
    }
    if (args.full) {
      cmd.push("--full")
    }
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    if (args.block) {
      cmd.push(args.block)
    }
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const calldata = tool({
  description: "ABI-encode a function with arguments",
  args: {
    sig: tool.schema.string().describe("The function signature"),
    args: tool.schema.array(tool.schema.string()).optional().describe("The arguments to encode"),
    file: tool.schema.string().optional().describe("The file containing the ABI"),
  },
  async execute(args) {
    const cmd = ["cast", "calldata"]
    if (args.file) {
      cmd.push("--file", args.file)
    }
    cmd.push(args.sig)
    if (args.args && args.args.length > 0) {
      cmd.push(...args.args)
    }
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const call = tool({
  description: "Perform a call on an account without publishing a transaction",
  args: {
    to: tool.schema.string().optional().describe("The contract address to call"),
    sig: tool.schema.string().optional().describe("The function signature to call"),
    args: tool.schema.array(tool.schema.string()).optional().describe("The arguments for the function call"),
    data: tool.schema.string().optional().describe("The hex-encoded data for the call"),
    block: tool.schema.string().optional().describe("The block height to query at. Can also be the tags earliest, finalized, safe, latest, or pending."),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
    trace: tool.schema.boolean().optional().describe("Trace the execution of the call"),
    value: tool.schema.string().optional().describe("The value to send with the call"),
    gasLimit: tool.schema.string().optional().describe("The gas limit for the call"),
    gasPrice: tool.schema.string().optional().describe("The gas price for the call"),
    from: tool.schema.string().optional().describe("The sender address for the call"),
  },
  async execute(args, context) {
    const cmd = ["cast", "call"]
    if (args.data) {
      cmd.push("--data", args.data)
    }
    if (args.trace) {
      cmd.push("--trace")
    }
    if (args.block) {
      cmd.push("--block", args.block)
    }
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    if (args.value) {
      cmd.push("--value", args.value)
    }
    if (args.gasLimit) {
      cmd.push("--gas-limit", args.gasLimit)
    }
    if (args.gasPrice) {
      cmd.push("--gas-price", args.gasPrice)
    }
    if (args.from) {
      cmd.push("--from", args.from)
    }
    if (args.to) {
      cmd.push(args.to)
    }
    if (args.sig) {
      cmd.push(args.sig)
    }
    if (args.args && args.args.length > 0) {
      cmd.push(...args.args)
    }
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const code = tool({
  description: "Get the runtime bytecode of a contract",
  args: {
    who: tool.schema.string().describe("The contract address"),
    block: tool.schema.string().optional().describe("The block height to query at"),
    disassemble: tool.schema.boolean().optional().describe("Disassemble bytecodes"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
  },
  async execute(args, context) {
    const cmd = ["cast", "code"]
    if (args.block) {
      cmd.push("--block", args.block)
    }
    if (args.disassemble) {
      cmd.push("--disassemble")
    }
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    cmd.push(args.who)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const concat_hex = tool({
  description: "Concatenate hex strings",
  args: {
    data: tool.schema.array(tool.schema.string()).optional().describe("The data to concatenate"),
  },
  async execute(args) {
    const cmd = ["cast", "concat-hex"]
    if (args.data && args.data.length > 0) {
      cmd.push(...args.data)
    }
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const decode_abi = tool({
  description: "Decode ABI-encoded input or output data",
  args: {
    sig: tool.schema.string().describe("The function signature in the format `<name>(<in-types>)(<out-types>)`"),
    calldata: tool.schema.string().describe("The ABI-encoded calldata"),
    input: tool.schema.boolean().optional().describe("Decode input data instead of output data"),
  },
  async execute(args) {
    const cmd = ["cast", "decode-abi"]
    if (args.input) cmd.push("--input")
    cmd.push(args.sig, args.calldata)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const decode_calldata = tool({
  description: "Decode ABI-encoded input data",
  args: {
    sig: tool.schema.string().describe("The function signature in the format `<name>(<in-types>)(<out-types>)`"),
    calldata: tool.schema.string().optional().describe("The ABI-encoded calldata"),
    file: tool.schema.string().optional().describe("Load ABI-encoded calldata from a file instead"),
  },
  async execute(args) {
    const cmd = ["cast", "decode-calldata"]
    if (args.file) cmd.push("--file", args.file)
    cmd.push(args.sig)
    if (args.calldata) cmd.push(args.calldata)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const decode_error = tool({
  description: "Decode custom error data",
  args: {
    data: tool.schema.string().describe("The error data to decode"),
    sig: tool.schema.string().optional().describe("The error signature. If none provided then tries to decode from local cache or https://api.openchain.xyz"),
  },
  async execute(args) {
    const cmd = ["cast", "decode-error"]
    if (args.sig) cmd.push("--sig", args.sig)
    cmd.push(args.data)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const decode_event = tool({
  description: "Decode event data",
  args: {
    data: tool.schema.string().describe("The event data to decode"),
    sig: tool.schema.string().optional().describe("The event signature. If none provided then tries to decode from local cache or https://api.openchain.xyz"),
  },
  async execute(args) {
    const cmd = ["cast", "decode-event"]
    if (args.sig) cmd.push("--sig", args.sig)
    cmd.push(args.data)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const decode_string = tool({
  description: "Decode ABI-encoded string",
  args: {
    data: tool.schema.string().describe("The ABI-encoded string"),
  },
  async execute(args) {
    const cmd = ["cast", "decode-string", args.data]
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const decode_transaction = tool({
  description: "Decodes a raw signed EIP 2718 typed transaction",
  args: {
    tx: tool.schema.string().optional().describe("The raw signed transaction to decode"),
  },
  async execute(args) {
    const cmd = ["cast", "decode-transaction"]
    if (args.tx) cmd.push(args.tx)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const erc20_balance = tool({
  description: "Query ERC20 token balance",
  args: {
    token: tool.schema.string().describe("The ERC20 token contract address"),
    owner: tool.schema.string().describe("The owner to query balance for"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
    block: tool.schema.string().optional().describe("The block height to query at"),
  },
  async execute(args, context) {
    const cmd = ["cast", "erc20-token", "balance"]
    if (args.block) cmd.push("--block", args.block)
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    cmd.push(args.token, args.owner)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const erc20_allowance = tool({
  description: "Query ERC20 token allowance",
  args: {
    token: tool.schema.string().describe("The ERC20 token contract address"),
    owner: tool.schema.string().describe("The owner address"),
    spender: tool.schema.string().describe("The spender address"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
    block: tool.schema.string().optional().describe("The block height to query at"),
  },
  async execute(args, context) {
    const cmd = ["cast", "erc20-token", "allowance"]
    if (args.block) cmd.push("--block", args.block)
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    cmd.push(args.token, args.owner, args.spender)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const erc20_name = tool({
  description: "Query ERC20 token name",
  args: {
    token: tool.schema.string().describe("The ERC20 token contract address"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
    block: tool.schema.string().optional().describe("The block height to query at"),
  },
  async execute(args, context) {
    const cmd = ["cast", "erc20-token", "name"]
    if (args.block) cmd.push("--block", args.block)
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    cmd.push(args.token)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const erc20_symbol = tool({
  description: "Query ERC20 token symbol",
  args: {
    token: tool.schema.string().describe("The ERC20 token contract address"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
    block: tool.schema.string().optional().describe("The block height to query at"),
  },
  async execute(args, context) {
    const cmd = ["cast", "erc20-token", "symbol"]
    if (args.block) cmd.push("--block", args.block)
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    cmd.push(args.token)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const erc20_decimals = tool({
  description: "Query ERC20 token decimals",
  args: {
    token: tool.schema.string().describe("The ERC20 token contract address"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
    block: tool.schema.string().optional().describe("The block height to query at"),
  },
  async execute(args, context) {
    const cmd = ["cast", "erc20-token", "decimals"]
    if (args.block) cmd.push("--block", args.block)
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    cmd.push(args.token)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const erc20_total_supply = tool({
  description: "Query ERC20 token total supply",
  args: {
    token: tool.schema.string().describe("The ERC20 token contract address"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
    block: tool.schema.string().optional().describe("The block height to query at"),
  },
  async execute(args, context) {
    const cmd = ["cast", "erc20-token", "total-supply"]
    if (args.block) cmd.push("--block", args.block)
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    cmd.push(args.token)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const find_block = tool({
  description: "Get the block number closest to the provided timestamp",
  args: {
    timestamp: tool.schema.string().describe("The UNIX timestamp to search for, in seconds"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
    insecure: tool.schema.boolean().optional().describe("Allow insecure RPC connections (accept invalid HTTPS certificates)"),
  },
  async execute(args, context) {
    const cmd = ["cast", "find-block"]
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    if (args.insecure) cmd.push("--insecure")
    cmd.push(args.timestamp)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const format_bytes32_string = tool({
  description: "Formats a string into bytes32 encoding",
  args: {
    string: tool.schema.string().optional().describe("The string to format into bytes32"),
  },
  async execute(args) {
    const cmd = ["cast", "format-bytes32-string"]
    if (args.string) {
      cmd.push(args.string)
    }
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const implementation = tool({
  description: "Fetch the EIP-1967 implementation address for a contract",
  args: {
    who: tool.schema.string().describe("The address for which the implementation will be fetched"),
    block: tool.schema.string().optional().describe("The block height to query at. Can also be the tags earliest, finalized, safe, latest, or pending."),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
  },
  async execute(args, context) {
    const cmd = ["cast", "implementation"]
    if (args.block) {
      cmd.push("--block", args.block)
    }
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    cmd.push(args.who)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const index = tool({
  description: "Compute the storage slot for an entry in a mapping",
  args: {
    keyType: tool.schema.string().describe("The mapping key type"),
    key: tool.schema.string().describe("The mapping key"),
    slotNumber: tool.schema.string().describe("The storage slot of the mapping"),
  },
  async execute(args) {
    const cmd = ["cast", "index", args.keyType, args.key, args.slotNumber]
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const keccak = tool({
  description: "Hash arbitrary data using Keccak-256",
  args: {
    data: tool.schema.string().optional().describe("The data to hash"),
  },
  async execute(args) {
    const cmd = ["cast", "keccak"]
    if (args.data) {
      cmd.push(args.data)
    }
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const logs = tool({
  description: "Get logs by signature or topic",
  args: {
    sigOrTopic: tool.schema.string().optional().describe("The signature of the event to filter logs by which will be converted to the first topic or a topic to filter on"),
    topicsOrArgs: tool.schema.array(tool.schema.string()).optional().describe("If used with a signature, the indexed fields of the event to filter by. Otherwise, the remaining topics of the filter"),
    fromBlock: tool.schema.string().optional().describe("The block height to start query at. Can also be the tags earliest, finalized, safe, latest, or pending."),
    toBlock: tool.schema.string().optional().describe("The block height to stop query at. Can also be the tags earliest, finalized, safe, latest, or pending."),
    address: tool.schema.string().optional().describe("The contract address to filter on"),
    etherscanApiKey: tool.schema.string().optional().describe("The Etherscan (or equivalent) API key"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
  },
  async execute(args, context) {
    const cmd = ["cast", "logs"]
    if (args.fromBlock) {
      cmd.push("--from-block", args.fromBlock)
    }
    if (args.toBlock) {
      cmd.push("--to-block", args.toBlock)
    }
    if (args.address) {
      cmd.push("--address", args.address)
    }
    if (args.etherscanApiKey) {
      cmd.push("--etherscan-api-key", args.etherscanApiKey)
    }
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    if (args.sigOrTopic) {
      cmd.push(args.sigOrTopic)
    }
    if (args.topicsOrArgs && args.topicsOrArgs.length > 0) {
      cmd.push(...args.topicsOrArgs)
    }
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const pretty_calldata = tool({
  description: "Pretty print calldata",
  args: {
    calldata: tool.schema.string().optional().describe("The calldata to pretty print"),
  },
  async execute(args) {
    const cmd = ["cast", "pretty-calldata"]
    if (args.calldata) cmd.push(args.calldata)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const receipt = tool({
  description: "Get the receipt for a transaction",
  args: {
    txHash: tool.schema.string().describe("The transaction hash"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
  },
  async execute(args, context) {
    const cmd = ["cast", "receipt"]
    cmd.push("--async")
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    cmd.push(args.txHash)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const rpc = tool({
  description: "Perform a raw JSON-RPC request",
  args: {
    method: tool.schema.string().describe("RPC method name"),
    params: tool.schema.array(tool.schema.string()).optional().describe("RPC parameters"),
    raw: tool.schema.boolean().optional().describe("Send raw JSON parameters"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
  },
  async execute(args, context) {
    const cmd = ["cast", "rpc"]
    if (args.raw) cmd.push("--raw")
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    cmd.push(args.method)
    if (args.params) cmd.push(...args.params)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const run = tool({
  description: "Runs a published transaction in a local environment and prints the trace",
  args: {
    txHash: tool.schema.string().describe("The transaction hash"),
    decodeInternal: tool.schema.boolean().optional().describe("Whether to identify internal functions in traces"),
    tracePrinter: tool.schema.boolean().optional().describe("Print out opcode traces"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
  },
  async execute(args, context) {
    const cmd = ["cast", "run"]
    if (args.decodeInternal) cmd.push("--decode-internal")
    if (args.tracePrinter) cmd.push("--trace-printer")
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    cmd.push(args.txHash)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const sig = tool({
  description: "Get the selector for a function",
  args: {
    sig: tool.schema.string().optional().describe("The function signature, e.g. transfer(address,uint256)"),
    optimize: tool.schema.string().optional().describe("Optimize signature to contain provided amount of leading zeroes in selector"),
  },
  async execute(args) {
    const cmd = ["cast", "sig"]
    if (args.sig) cmd.push(args.sig)
    if (args.optimize) cmd.push(args.optimize)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const sig_event = tool({
  description: "Generate event signatures from event string",
  args: {
    eventString: tool.schema.string().optional().describe("The event string to generate a signature for"),
  },
  async execute(args) {
    const cmd = ["cast", "sig-event"]
    if (args.eventString) {
      cmd.push(args.eventString)
    }
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const source = tool({
  description: "Get the source code of a contract from a block explorer",
  args: {
    address: tool.schema.string().describe("The contract address"),
    flatten: tool.schema.boolean().optional().describe("Flatten the source code"),
  },
  async execute(args) {
    const cmd = ["cast", "source"]
    if (args.flatten) {
      cmd.push("--flatten")
    }
    cmd.push(args.address)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const storage = tool({
  description: "Get the raw value of a contract's storage slot",
  args: {
    address: tool.schema.string().describe("The contract address"),
    baseSlot: tool.schema.string().optional().describe("The base storage slot"),
    offset: tool.schema.string().optional().describe("The offset from the base slot"),
  },
  async execute(args) {
    const cmd = ["cast", "storage"]
    cmd.push(args.address)
    if (args.baseSlot) {
      cmd.push(args.baseSlot)
    }
    if (args.offset) {
      cmd.push(args.offset)
    }
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})

export const tx = tool({
  description: "Get information about a transaction",
  args: {
    txHash: tool.schema.string().describe("The transaction hash"),
    chain: tool.schema.string().optional().describe("The chain name or EIP-155 chain ID"),
  },
  async execute(args, context) {
    const cmd = ["cast", "tx"]
    cmd.push("--rpc-url",
      args.chain ?
        await resolveRpcUrl(args.chain, context.worktree)
        :
        await resolveRpcUrl("1", context.worktree)
    )
    cmd.push(args.txHash)
    const result = await bunRuntime.$`${cmd}`.text()
    return result.trim()
  },
})
