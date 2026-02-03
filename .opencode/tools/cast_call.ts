import { tool } from "@opencode-ai/plugin";
import { getRpcUrl } from "./defaults";

export default tool({
  description: "Call a contract's view function without publishing a transaction. Use this to query contract state like slot0(), liquidity(), balanceOf(), or any read-only function. Requires Foundry to be installed. The default RPC URL is fetched from defaults.ts if not specified.",
  args: {
    address: tool.schema
      .string()
      .describe("The contract address to call (e.g., 0x1234...)")
      .regex(/^0x[a-fA-F0-9]{40}$/),
    sig: tool.schema
      .string()
      .describe("Function signature (e.g., 'slot0()', 'balanceOf(address)', 'getReserves()')"),
    args: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("Function arguments as strings (e.g., ['0x1234...', '1000000'])"),
    chain: tool.schema
      .string()
      .optional()
      .describe("Chain name (e.g., 'mainnet', 'arbitrum', 'base') or EIP-155 chain ID (e.g., '1', '42161'). Defaults to mainnet"),
    block: tool.schema
      .string()
      .optional()
      .describe("Block number to query at (defaults to latest)"),
    rpc_url: tool.schema
      .string()
      .optional()
      .describe("Custom RPC URL to use instead of the default from defaults.ts"),
  },
  async execute(params: any, context: any) {
    const cmd_parts = ["cast", "call"];

    if (params.chain) {
      cmd_parts.push("--chain", params.chain);
    }

    if (params.block) {
      cmd_parts.push("--block", params.block);
    }

    cmd_parts.push("--rpc-url", getRpcUrl(params.chain, params.rpc_url));

    // Add contract address
    cmd_parts.push(params.address);

    // Add function signature
    cmd_parts.push(params.sig);

    // Add function arguments if provided
    if (params.args && params.args.length > 0) {
      cmd_parts.push(...params.args);
    }

    try {
      // @ts-expect-error Bun runtime is available
      const result = await Bun.$`${cmd_parts}`.text();
      return result.trim();
    } catch (error: any) {
      if (error.stderr) {
        throw new Error(`cast call failed: ${error.stderr}`);
      }
      throw new Error(`cast call failed: ${error.message || error}`);
    }
  },
});
