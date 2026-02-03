import { tool } from "@opencode-ai/plugin";

export default tool({
  description: "Get the native token balance (ETH on Ethereum, or equivalent on other chains) of an account. Use this to check wallet or contract balances during debugging. Requires Foundry to be installed.",
  args: {
    address: tool.schema
      .string()
      .describe("The account address to check (e.g., 0x1234...)")
      .regex(/^0x[a-fA-F0-9]{40}$/),
    chain: tool.schema
      .string()
      .optional()
      .describe("Chain name (e.g., 'mainnet', 'arbitrum', 'base') or EIP-155 chain ID. Defaults to mainnet"),
    block: tool.schema
      .string()
      .optional()
      .describe("Block number to query at (defaults to latest)"),
    rpc_url: tool.schema
      .string()
      .optional()
      .describe("Custom RPC URL to use instead of the default for the chain"),
  },
  async execute(params: any, context: any) {
    const cmd_parts = ["cast", "balance"];

    if (params.chain) {
      cmd_parts.push("--chain", params.chain);
    }

    if (params.block) {
      cmd_parts.push("--block", params.block);
    }

    if (params.rpc_url) {
      cmd_parts.push("--rpc-url", params.rpc_url);
    }

    // Add the address
    cmd_parts.push(params.address);

    try {
      // @ts-expect-error Bun runtime is available
      const result = await Bun.$`${cmd_parts}`.text();
      return result.trim();
    } catch (error: any) {
      if (error.stderr) {
        throw new Error(`cast balance failed: ${error.stderr}`);
      }
      throw new Error(`cast balance failed: ${error.message || error}`);
    }
  },
});
