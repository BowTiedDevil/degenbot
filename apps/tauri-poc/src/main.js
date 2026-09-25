import { invoke } from "@tauri-apps/api/core";
import "./style.css";
import { listen } from "@tauri-apps/api/event";

const MAX_BLOCKS = 40;
const MAX_LOGS = 80;
const blocks = [];
const logs = [];
let latestBlock;

const elements = {
  statusDot: document.querySelector("#status-dot"),
  statusLabel: document.querySelector("#status-label"),
  latestBlock: document.querySelector("#latest-block"),
  latestTransactions: document.querySelector("#latest-transactions"),
  latestLogs: document.querySelector("#latest-logs"),
  latestBaseFee: document.querySelector("#latest-base-fee"),
  errorPanel: document.querySelector("#error-panel"),
  errorMessage: document.querySelector("#error-message"),
  blockRows: document.querySelector("#block-rows"),
  logRows: document.querySelector("#log-rows"),
  blockCount: document.querySelector("#block-count"),
  logCount: document.querySelector("#log-count"),
};

function formatNumber(value) {
  return typeof value === "number" ? value.toLocaleString() : "—";
}

function formatBaseFee(value) {
  if (typeof value !== "number") return "—";
  return `${(value / 1_000_000_000).toFixed(3)} gwei`;
}

function formatAge(timestamp) {
  if (typeof timestamp !== "number" || timestamp === 0) return "—";
  const seconds = Math.max(0, Math.floor(Date.now() / 1000 - timestamp));
  if (seconds < 60) return `${seconds}s`;
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

function setStatus(status) {
  elements.statusLabel.textContent = status;
  elements.statusDot.className = `status-dot ${status}`;
}

function showError(message) {
  elements.errorMessage.textContent = message;
  elements.errorPanel.classList.remove("hidden");
}

function clearError() {
  elements.errorMessage.textContent = "";
  elements.errorPanel.classList.add("hidden");
}

function upsertBlock(block) {
  const index = blocks.findIndex((candidate) => candidate.block_number === block.block_number);
  if (index === -1) blocks.unshift(block);
  else blocks[index] = block;
  blocks.sort((left, right) => right.block_number - left.block_number);
  blocks.splice(MAX_BLOCKS);
  latestBlock = blocks[0];
  renderBlocks();
  renderSummary();
}

function addLog(log) {
  logs.unshift(log);
  logs.splice(MAX_LOGS);
  renderLogs();
}

function renderSummary() {
  if (!latestBlock) return;
  elements.latestBlock.textContent = formatNumber(latestBlock.block_number);
  elements.latestTransactions.textContent = formatNumber(latestBlock.transaction_count);
  elements.latestLogs.textContent = formatNumber(latestBlock.log_count);
  elements.latestBaseFee.textContent = formatBaseFee(latestBlock.base_fee_wei);
}

function renderBlocks() {
  elements.blockCount.textContent = `${blocks.length} block${blocks.length === 1 ? "" : "s"}`;
  if (blocks.length === 0) return;
  elements.blockRows.innerHTML = blocks.map((block) => `
    <tr>
      <td class="mono emphasis">${block.block_number}</td>
      <td>${formatAge(block.block_timestamp)}</td>
      <td>${formatNumber(block.transaction_count)}</td>
      <td>${formatNumber(block.log_count)}</td>
      <td>${formatBaseFee(block.base_fee_wei)}</td>
      <td>${formatBaseFee(block.next_base_fee_wei)}</td>
      <td class="mono">${formatNumber(block.gas_used)} / ${formatNumber(block.gas_limit)}</td>
    </tr>
  `).join("");
}

function renderLogs() {
  elements.logCount.textContent = `${logs.length} log${logs.length === 1 ? "" : "s"}`;
  if (logs.length === 0) return;
  elements.logRows.innerHTML = logs.map((log) => `
    <tr>
      <td class="mono">${formatNumber(log.block_number)}</td>
      <td class="mono">${formatNumber(log.log_index)}</td>
      <td class="mono">${log.address ?? "—"}</td>
      <td class="mono topic">${log.topic0 ?? "—"}</td>
      <td>${log.removed ? "yes" : "no"}</td>
    </tr>
  `).join("");
}

function demoBlock(offset) {
  const blockNumber = 21_500_000 - offset;
  const gasUsed = 14_000_000 + offset * 125_000;
  const baseFee = 1_250_000_000 + offset * 12_500_000;
  const nextBaseFee = baseFee - Math.floor(baseFee * (gasUsed < 15_000_000 ? 0.01 : -0.01) / 8);
  return {
    block_number: blockNumber,
    block_timestamp: Math.floor(Date.now() / 1000) - offset * 12,
    transaction_count: 142 - offset * 3,
    log_count: 38 + offset * 4,
    base_fee_wei: baseFee,
    next_base_fee_wei: nextBaseFee,
    gas_used: gasUsed,
    gas_limit: 30_000_000,
  };
}

function runDemo() {
  setStatus("running");
  for (let offset = 6; offset >= 0; offset -= 1) {
    upsertBlock(demoBlock(offset));
  }
  for (let offset = 0; offset < 8; offset += 1) {
    addLog({
      block_number: demoBlock(offset).block_number,
      log_index: offset,
      address: "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640",
      topic0: "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
      removed: false,
    });
  }
}

async function main() {
  await listen("feed-event", ({ payload }) => {
    switch (payload.type) {
      case "status":
        setStatus(payload.status);
        if (payload.status === "running") clearError();
        break;
      case "block":
        upsertBlock(payload.block);
        break;
      case "log":
        addLog(payload.log);
        break;
      case "error":
        showError(payload.message);
        break;
      default:
        break;
    }
  });

  await invoke("start_feed");
  clearError();
}

if (new URLSearchParams(window.location.search).has("demo")) {
  runDemo();
} else {
  main().catch((error) => {
    setStatus("error");
    showError(String(error));
  });
}
