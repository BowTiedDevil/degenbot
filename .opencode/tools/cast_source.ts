import { tool } from "@opencode-ai/plugin";

export default tool({
  description: "Fetch verified smart contract source code from Etherscan or compatible block explorers. Use this when you need to examine or analyze contract code at a specific address. Requires Foundry to be installed.",
  args: {
    address: tool.schema
      .string()
      .describe("The contract address (e.g., 0x1234...)")
      .regex(/^0x[a-fA-F0-9]{40}$/),
    chain: tool.schema
      .string()
      .optional()
      .describe("Chain name (e.g., 'mainnet', 'sepolia') or EIP-155 chain ID (e.g., '1', '11155111'). Defaults to mainnet if not specified"),
    output_dir: tool.schema
      .string()
      .optional()
      .describe("Directory path to save the expanded source tree. If provided, source files will be saved to this directory"),
    flatten: tool.schema
      .boolean()
      .optional()
      .describe("Whether to flatten the source code into a single file instead of keeping the original structure"),
    etherscan_api_key: tool.schema
      .string()
      .optional()
      .describe("Etherscan API key (optional - uses ETHERSCAN_API_KEY env var by default)"),
    explorer_api_url: tool.schema
      .string()
      .optional()
      .describe("Alternative block explorer API URL that adheres to the Etherscan API format"),
  },
  async execute(args: any, context: any) {
    const cmd_parts = ["cast", "source"];

    // Add optional flags
    if (args.flatten) {
      cmd_parts.push("--flatten");
    }

    if (args.output_dir) {
      cmd_parts.push("-d", args.output_dir);
    }

    if (args.chain) {
      cmd_parts.push("--chain", args.chain);
    }

    if (args.etherscan_api_key) {
      cmd_parts.push("--etherscan-api-key", args.etherscan_api_key);
    }

    if (args.explorer_api_url) {
      cmd_parts.push("--explorer-api-url", args.explorer_api_url);
    }

    // Add the required address argument last
    cmd_parts.push(args.address);

    try {
      // @ts-expect-error Bun runtime is available
      const result = await Bun.$`${cmd_parts}`.text();
      return result.trim();
    } catch (error: any) {
      if (error.stderr) {
        throw new Error(`cast source failed: ${error.stderr}`);
      }
      throw new Error(`cast source failed: ${error.message || error}`);
    }
  },
});
