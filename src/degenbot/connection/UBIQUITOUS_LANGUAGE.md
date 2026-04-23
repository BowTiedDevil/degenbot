# Ubiquitous Language — Infrastructure

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Anvil Fork** | A local forked blockchain instance running via Foundry's Anvil client for testing | Fork, local chain |
| **Provider** | An adapter wrapping an RPC connection for blockchain reads (sync or async) | RPC client, web3 |
| **Connection Manager** | A singleton managing provider instances keyed by chain ID | Connection |
| **Pool State Message** | A publisher/subscriber message notifying that a pool's state has changed | State update message |
