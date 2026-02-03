import { tool } from "@opencode-ai/plugin";
import { getRpcUrl } from "./defaults";

export default tool({
  description: "Read any raw storage slot value from a smart contract. Use this for debugging contract state or inspecting arbitrary storage positions. For EIP-1967 proxy contracts (implementation/beacon/admin), prefer the get_eip1967_address tool instead. Requires Foundry to be installed. The default RPC URL is fetched from defaults.ts if not specified.",
  args: {
    address: tool.schema
      .string()
      .describe("The contract address (e.g., 0x1234...)")
      .regex(/^0x[a-fA-F0-9]{40}$/),
    slot: tool.schema
      .string()
      .describe("The storage slot to read. Can be hex (0x1234...) or decimal (42)"),
    chain: tool.schema
      .string()
      .optional()
      .describe("Chain name (e.g., 'mainnet', 'sepolia') or EIP-155 chain ID. Defaults to mainnet"),
    block: tool.schema
      .string()
      .optional()
      .describe("Block number to query at (defaults to latest)"),
    rpc_url: tool.schema
      .string()
      .optional()
      .describe("Custom RPC URL to use instead of the default from defaults.ts"),
  },
  async execute(args: any, context: any) {
    const cmd_parts = ["cast", "storage"];

    if (args.chain) {
      cmd_parts.push("--chain", args.chain);
    }

    if (args.block) {
      cmd_parts.push("--block", args.block);
    }

    cmd_parts.push("--rpc-url", getRpcUrl(args.chain, args.rpc_url));

    // Add required arguments
    cmd_parts.push(args.address, args.slot);

    try {
      // @ts-expect-error Bun runtime is available
      const result = await Bun.$`${cmd_parts}`.text();
      return result.trim();
    } catch (error: any) {
      if (error.stderr) {
        throw new Error(`cast storage failed: ${error.stderr}`);
      }
      throw new Error(`cast storage failed: ${error.message || error}`);
    }
  },
});
