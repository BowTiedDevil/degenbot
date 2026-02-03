import { tool } from "@opencode-ai/plugin";
import { getRpcUrl } from "./defaults";

export default tool({
  description: "Fetch event logs from the blockchain. Use this to query Swap, Mint, Burn, PoolCreated, or other contract events. Requires Foundry to be installed. The default RPC URL is fetched from defaults.ts if not specified.",
  args: {
    address: tool.schema
      .string()
      .optional()
      .describe("Contract address to filter logs by (e.g., 0x1234...). Optional - omit to get all matching events"),
    signature: tool.schema
      .string()
      .optional()
      .describe("Event signature (e.g., 'Swap(address,address,int256,int256,uint160,uint128,int24)'). Either signature or topic0 must be provided"),
    topic0: tool.schema
      .string()
      .optional()
      .describe("Event topic 0 hash (e.g., '0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67'). Alternative to signature"),
    from_block: tool.schema
      .string()
      .describe("Starting block number (use 'latest' for most recent block, or specific number like '1000000')"),
    to_block: tool.schema
      .string()
      .optional()
      .describe("Ending block number (defaults to latest)"),
    chain: tool.schema
      .string()
      .optional()
      .describe("Chain name (e.g., 'mainnet', 'arbitrum', 'base') or EIP-155 chain ID. Defaults to mainnet"),
    rpc_url: tool.schema
      .string()
      .optional()
      .describe("Custom RPC URL to use instead of the default from defaults.ts"),
  },
  async execute(params: any, context: any) {
    const cmd_parts = ["cast", "logs"];

    if (params.chain) {
      cmd_parts.push("--chain", params.chain);
    }

    cmd_parts.push("--rpc-url", getRpcUrl(params.chain, params.rpc_url));

    // Add from block (required)
    cmd_parts.push("--from-block", params.from_block);

    // Add to block if provided
    if (params.to_block) {
      cmd_parts.push("--to-block", params.to_block);
    }

    // Add address filter if provided
    if (params.address) {
      cmd_parts.push("--address", params.address);
    }

    // Add signature or topic0
    if (params.signature) {
      cmd_parts.push(params.signature);
    } else if (params.topic0) {
      cmd_parts.push(params.topic0);
    } else {
      throw new Error("Either 'signature' or 'topic0' must be provided");
    }

    try {
      // @ts-expect-error Bun runtime is available
      const result = await Bun.$`${cmd_parts}`.text();
      return result.trim();
    } catch (error: any) {
      if (error.stderr) {
        throw new Error(`cast logs failed: ${error.stderr}`);
      }
      throw new Error(`cast logs failed: ${error.message || error}`);
    }
  },
});
