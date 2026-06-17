#!/usr/bin/env bash
# Render a Mermaid diagram to PNG/SVG/CDN/etc.
#
# Usage:
#   scripts/mermaid-export <input.md|input.mmd> [output] [-f png|svg|pdf]
#
# - Input may be a `.mmd` file (used as-is) or a Markdown file from which the
#   *first* fenced ```mermaid block is extracted.
# - Output defaults to the input's basename (with .png, or the chosen format)
#   written next to the source.
# - Pixel density (-s/--scale) defaults to $MERMAID_SCALE or 3 (the Puppeteer
#   deviceScaleFactor; raise for crisper/higher-DPI PNGs, e.g. 4 or 5).
# - Chromium is auto-detected from $MERMAID_CHROMIUM or the first available
#   chromium-browser / google-chrome on PATH. The bundled puppeteer browser is
#   never used (we built the CLI with PUPPETEER_SKIP_DOWNLOAD=true).
#
# Requires the `mmdc` CLI from @mermaid-js/mermaid-cli.
set -euo pipefail

FORMAT="png"
SCALE="${MERMAID_SCALE:-3}"
INPUT=""
OUTPUT=""

usage() {
  sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    -f|--format) FORMAT="$2"; shift 2 ;;
    -s|--scale) SCALE="$2"; shift 2 ;;
    -h|--help) usage 0 ;;
    -*) echo "unknown option: $1" >&2; usage 1 ;;
    *)
      if [ -z "$INPUT" ]; then INPUT="$1"
      elif [ -z "$OUTPUT" ]; then OUTPUT="$1"
      else echo "unexpected extra argument: $1" >&2; exit 1
      fi
      shift ;;
  esac
done

if [ -z "$INPUT" ]; then usage 1; fi
if ! command -v mmdc >/dev/null 2>&1; then
  echo "error: 'mmdc' not found on PATH." >&2
  echo "install with: PUPPETEER_SKIP_DOWNLOAD=true npm install -g @mermaid-js/mermaid-cli" >&2
  exit 1
fi

# --- Resolve a headless Chromium executable -------------------------------
CHROME="${MERMAID_CHROMIUM:-}"
if [ -z "$CHROME" ]; then
  for c in chromium chromium-browser google-chrome google-chrome-stable; do
    if command -v "$c" >/dev/null 2>&1; then CHROME="$(command -v "$c")"; break; fi
  done
fi
if [ -z "$CHROME" ]; then
  echo "error: no chromium/chrome found. Set \$MERMAID_CHROMIUM or install one." >&2
  exit 1
fi

# --- Source diagram (.mmd as-is, or extract first ```mermaid block) -------
TMPDIR_WORK="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_WORK"' EXIT

case "$INPUT" in
  *.mmd) SRC="$INPUT" ;;
  *.md|*.markdown)
    SRC="$TMPDIR_WORK/diagram.mmd"
    awk '
      /^```mermaid$/ { cap=1; next }
      /^```[[:space:]]*$/ { if(cap){cap=0; exit} }
      cap { print }
    ' "$INPUT" > "$SRC"
    if [ ! -s "$SRC" ]; then
      echo "error: no fenced \`\`\`mermaid block found in $INPUT" >&2
      exit 1
    fi
    ;;
  *) echo "error: input must be a .mmd or .md file (got: $INPUT)" >&2; exit 1 ;;
esac

# --- Output path ---------------------------------------------------------
if [ -z "$OUTPUT" ]; then
  base="${INPUT##*/}"; base="${base%.*}"
  OUTPUT="${base}.${FORMAT}"
fi

# --- Puppeteer config (point at system chromium) -------------------------
PUPPETEER_CFG="$TMPDIR_WORK/puppeteer.json"
cat > "$PUPPETEER_CFG" <<EOF
{
  "executablePath": "$CHROME",
  "args": ["--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage", "--disable-setuid-sandbox"]
}
EOF

echo "mermaid-export: $INPUT → $OUTPUT (${FORMAT}, scale=${SCALE}, chrome=$(basename "$CHROME"))"
mmdc \
  --input "$SRC" \
  --output "$OUTPUT" \
  --puppeteerConfigFile "$PUPPETEER_CFG" \
  --backgroundColor white \
  --scale "$SCALE" \
  1>&2

echo "wrote $OUTPUT"
