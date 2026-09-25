import { spawn } from "node:child_process";

const args = process.argv.slice(2);
const headlessDev = args[0] === "dev" && !process.env.DISPLAY && !process.env.WAYLAND_DISPLAY;
const command = headlessDev ? "xvfb-run" : "tauri";
const commandArgs = headlessDev
  ? ["-a", "-s", "-screen 0 1600x1000x24", "tauri", ...args]
  : args;
const child = spawn(command, commandArgs, {
  env: process.env,
  stdio: "inherit",
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => child.kill(signal));
}

child.on("error", (error) => {
  process.stderr.write(`failed to launch ${command}: ${error.message}\n`);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 1);
  }
});
