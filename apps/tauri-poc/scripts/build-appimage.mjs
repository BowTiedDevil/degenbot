import { spawn } from "node:child_process";

const child = spawn(
  process.execPath,
  ["scripts/tauri.mjs", "build", "-b", "appimage"],
  {
    env: {
      ...process.env,
      APPIMAGE_EXTRACT_AND_RUN: "1",
      NO_STRIP: "1",
    },
    stdio: "inherit",
  },
);

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => child.kill(signal));
}

child.on("error", (error) => {
  process.stderr.write(`failed to launch AppImage build: ${error.message}\n`);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 1);
  }
});
