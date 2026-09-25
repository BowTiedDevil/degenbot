# degenbot Tauri feed PoC

This is a small native Tauri shell around the pure-Rust `degenbot` facade. It starts a Rust-owned Tokio runtime, subscribes to `newHeads` and `logs` through `degenbot-ingestion`, enriches headers with transaction counts, and streams typed events to the webview.

## Run

Set the WebSocket endpoint before launching the app:

```bash
export DEGENBOT_RPC_WS_CHAINID_1=ws://127.0.0.1:8546
cd apps/tauri-poc
npm install
npm run tauri dev
```

The GUI calls the canonical `degenbot-config` node resolver. For this mainnet
PoC it resolves chain `1` through `DEGENBOT_RPC_WS_CHAINID_1`. Node endpoints are
intentionally not read from `config.toml`: the retired `[ws]`/`[rpc]` file
vocabulary is deliberately refused by the Rust configuration architecture. The
`ETHEREUM_ARCHIVE_NODE_WS_URI` name is a test-environment variable, not a
canonical Rust configuration key.

When launching the packaged AppImage from a desktop session, pass the variable
explicitly if the desktop environment does not inherit your shell:

```bash
DEGENBOT_RPC_WS_CHAINID_1=ws://127.0.0.1:8546 \
  './degenbot feed PoC_0.1.0_amd64.AppImage'
```

The `npm run tauri dev` script automatically uses `xvfb-run` when neither `DISPLAY` nor `WAYLAND_DISPLAY` is available. In a normal desktop session it launches Tauri directly.

## AppImage

AppImage packaging is available as a first-class npm command:

```bash
cd apps/tauri-poc
npm run appimage
```

The command enables AppImage extraction mode and `NO_STRIP=1` for linuxdeploy compatibility with modern Fedora libraries that use `.relr.dyn` ELF sections. The resulting AppImage is written under `src-tauri/target/release/bundle/appimage/`.

## Browser-only visual loop

The frontend has a deterministic fixture mode that does not require Tauri, a node, or native GUI libraries:

```bash
cd apps/tauri-poc
npm run dev
# open http://127.0.0.1:1420/?demo=1
```

This mode is intended for browser automation and screenshots. The real app path uses Tauri's `feed-event` channel and the Rust ingestion runtime.

## Checks

```bash
cargo test --manifest-path apps/tauri-poc/feed-model/Cargo.toml
cd apps/tauri-poc && npm run build
```

A native Tauri build additionally needs the platform WebViewGTK/WebKit development packages. On Fedora, the prerequisites are:

```bash
sudo dnf install webkit2gtk4.1-devel openssl-devel curl wget file libappindicator-gtk3-devel librsvg2-devel libxdo-devel
sudo dnf group install "c-development"
```

In a headless shell, install the virtual X11 server once:

```bash
sudo dnf install xorg-x11-server-Xvfb xorg-x11-server-Xorg xorg-x11-xauth
```

After that, the normal command is sufficient:

```bash
cd apps/tauri-poc
npm run tauri dev
```

The wrapper detects the missing display and runs Tauri under a 1600x1000 Xvfb display. To force this mode explicitly:

```bash
xvfb-run -a -s "-screen 0 1600x1000x24" npm run tauri dev
```
