import { tool } from "@opencode-ai/plugin";

// Foundry's supported chains based on their documentation
const SUPPORTED_CHAINS = [
  { name: "mainnet", id: "1" },
  { name: "sepolia", id: "11155111" },
  { name: "holesky", id: "17000" },
  { name: "arbitrum", id: "42161" },
  { name: "arbitrum-sepolia", id: "421614" },
  { name: "optimism", id: "10" },
  { name: "optimism-sepolia", id: "11155420" },
  { name: "base", id: "8453" },
  { name: "base-sepolia", id: "84532" },
  { name: "polygon", id: "137" },
  { name: "polygon-amoy", id: "80002" },
  { name: "avalanche", id: "43114" },
  { name: "avalanche-fuji", id: "43113" },
  { name: "bnb", id: "56" },
  { name: "bnb-testnet", id: "97" },
  { name: "gnosis", id: "100" },
  { name: "fantom", id: "250" },
  { name: "fantom-testnet", id: "4002" },
  { name: "moonbeam", id: "1284" },
  { name: "moonriver", id: "1285" },
  { name: "moonbase", id: "1287" },
  { name: "scroll", id: "534352" },
  { name: "scroll-sepolia", id: "534351" },
  { name: "metis", id: "1088" },
  { name: "linea", id: "59144" },
  { name: "linea-sepolia", id: "59141" },
  { name: "zksync", id: "324" },
  { name: "zksync-sepolia", id: "300" },
  { name: "mantle", id: "5000" },
  { name: "mantle-sepolia", id: "5003" },
  { name: "blast", id: "81457" },
  { name: "blast-sepolia", id: "168587773" },
  { name: "mode", id: "34443" },
  { name: "mode-sepolia", id: "919" },
];

export default tool({
  description: "List all supported blockchain chains with their names and IDs. Use this to discover valid chain names or IDs before using other cast tools. Returns a list of chain objects with 'name' and 'id' properties.",
  args: {},
  async execute(_params: any, _context: any) {
    return JSON.stringify(SUPPORTED_CHAINS, null, 2);
  },
});
