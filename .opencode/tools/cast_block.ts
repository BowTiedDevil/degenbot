import { tool } from "@opencode-ai/plugin";
import { getRpcUrl } from "./defaults";

export default tool({
  description: "Get information about a block (timestamp, gas used, base fee, etc.). Use this to verify block timing, check gas prices, or debug event ordering. Requires Foundry to be installed. The default RPC URL is fetched from defaults.ts if not specified.",
  args: {
    block: tool.schema
      .string()
      .describe("Block identifier: number (e.g., '18000000'), hash (0x...), or keyword ('latest', 'earliest', 'pending')"),
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
    const cmd_parts = ["cast", "block"];

    if (params.chain) {
      cmd_parts.push("--chain", params.chain);
    }

    cmd_parts.push("--rpc-url", getRpcUrl(params.chain, params.rpc_url));

    // Add the block identifier
    cmd_parts.push(params.block);

    try {
      // @ts-expect-error Bun runtime is available
      const result = await Bun.$`${cmd_parts}`.text();
      return result.trim();
    } catch (error: any) {
      if (error.stderr) {
        throw new Error(`cast block failed: ${error.stderr}`);
      }
      throw new Error(`cast block failed: ${error.message || error}`);
    }
  },
});
