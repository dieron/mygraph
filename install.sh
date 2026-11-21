#!/bin/sh
# Installs MyGraph from a GitHub release on Linux or macOS and runs it as a background service.
#
#   curl -fsSL https://raw.githubusercontent.com/dieron/mygraph/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/dieron/mygraph/main/install.sh | sh -s -- --no-service
#
# What it does:
#   - downloads the archive for this platform and the embedding model, and checks both against
#     the release's SHA256SUMS;
#   - installs into ~/.local/share/mygraph (bin/, data/, server.toml) and links ~/.local/bin/mygraph;
#   - writes server.toml on first install (listen 127.0.0.1:7420, no keys: only this machine
#     can reach it) and keeps it, and the data, on upgrades;
#   - starts the server at login: a systemd user service on Linux, a launchd agent on macOS.
# Running it again upgrades. Nothing is sent anywhere except the downloads from github.com.
#
# Options (or environment variables):
#   --version X.Y.Z   MYGRAPH_VERSION   a release (default: the latest)
#   --dir DIR         MYGRAPH_HOME      where to install (default: ~/.local/share/mygraph)
#   --port N          MYGRAPH_PORT      port on 127.0.0.1 (default: 7420; first install only)
#   --no-model        MYGRAPH_NO_MODEL=1    skip the model (162 MB): words-only search
#   --no-service      MYGRAPH_NO_SERVICE=1  install only; start it yourself with `mygraph serve`
#   --uninstall       stop and remove the service and the link; the folder and data stay
set -eu

REPO=dieron/mygraph
MODEL=multilingual-e5-small
VERSION=${MYGRAPH_VERSION:-}
HOME_DIR=${MYGRAPH_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/mygraph}
PORT=${MYGRAPH_PORT:-7420}
NO_MODEL=${MYGRAPH_NO_MODEL:-}
NO_SERVICE=${MYGRAPH_NO_SERVICE:-}
UNINSTALL=
LINK_DIR=$HOME/.local/bin
UNIT=mygraph.service
AGENT=io.github.dieron.mygraph

say() { printf '%s\n' "$*"; }
fail() { printf 'mygraph install: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case $1 in
    --version) VERSION=${2:?--version needs a value}; shift 2 ;;
    --dir) HOME_DIR=${2:?--dir needs a value}; shift 2 ;;
    --port) PORT=${2:?--port needs a value}; shift 2 ;;
    --no-model) NO_MODEL=1; shift ;;
    --no-service) NO_SERVICE=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) say "Options: --version X.Y.Z, --dir DIR, --port N, --no-model, --no-service, --uninstall"
      say "Details: https://github.com/$REPO/blob/main/install.sh"; exit 0 ;;
    *) fail "unknown option $1 (see --help)" ;;
  esac
done
VERSION=${VERSION#v}
BIN=$HOME_DIR/bin
EXE=$BIN/mygraph
CONFIG=$HOME_DIR/server.toml
OS=$(uname -s)

need() { command -v "$1" >/dev/null 2>&1 || fail "needs $1"; }
need curl
need tar
need uname

# --- service helpers ---------------------------------------------------------------------------

has_systemd() { command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; }
PLIST=$HOME/Library/LaunchAgents/$AGENT.plist

stop_service() {
  case $OS in
    Linux) if has_systemd; then systemctl --user stop "$UNIT" 2>/dev/null || true; fi ;;
    Darwin) launchctl bootout "gui/$(id -u)/$AGENT" 2>/dev/null || true ;;
  esac
}

if [ -n "$UNINSTALL" ]; then
  stop_service
  case $OS in
    Linux)
      if has_systemd; then systemctl --user disable "$UNIT" 2>/dev/null || true; fi
      rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$UNIT"
      if has_systemd; then systemctl --user daemon-reload; fi ;;
    Darwin) rm -f "$PLIST" ;;
  esac
  [ -L "$LINK_DIR/mygraph" ] && rm -f "$LINK_DIR/mygraph"
  say "MyGraph stopped and removed from autostart. Kept: $HOME_DIR (program, data, server.toml)."
  exit 0
fi

# --- platform ----------------------------------------------------------------------------------

case $OS-$(uname -m) in
  Linux-x86_64|Linux-amd64) TARGET=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) TARGET=aarch64-unknown-linux-gnu ;;
  Darwin-arm64) TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64) fail "no build for Intel Macs yet; use Docker (see the README) or build from source" ;;
  *) fail "no build for $OS $(uname -m); use Docker (see the README) or build from source" ;;
esac

if [ "$OS" = Linux ]; then
  glibc=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}') || glibc=
  if [ -z "$glibc" ]; then
    fail "the Linux builds need glibc 2.38 or newer (this looks like musl, e.g. Alpine); use Docker"
  fi
  major=${glibc%%.*}; minor=${glibc#*.}; minor=${minor%%.*}
  if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 38 ]; }; then
    fail "the Linux builds need glibc 2.38 or newer (Ubuntu 24.04, Debian 13); this has $glibc. Use Docker"
  fi
fi

if command -v sha256sum >/dev/null 2>&1; then
  sha() { sha256sum "$1" | awk '{print $1}'; }
else
  need shasum
  sha() { shasum -a 256 "$1" | awk '{print $1}'; }
fi

# --- download ----------------------------------------------------------------------------------

if [ -z "$VERSION" ]; then
  url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") \
    || fail "cannot reach github.com"
  VERSION=${url##*/v}
  case $VERSION in [0-9]*) ;; *) fail "cannot find the latest release ($url)" ;; esac
fi
BASE=https://github.com/$REPO/releases/download/v$VERSION
PACKAGE=mygraph-$VERSION-$TARGET
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM

get() {
  say "downloading $1"
  curl -fSL --progress-bar -o "$TMP/$1" "$BASE/$1" || fail "download failed: $BASE/$1"
}
check() {
  want=$(awk -v f="$1" '$2 == f || $2 == "*"f {print $1}' "$TMP/SHA256SUMS")
  [ -n "$want" ] || fail "$1 is not listed in SHA256SUMS"
  [ "$(sha "$TMP/$1")" = "$want" ] || fail "checksum mismatch for $1"
}

say "MyGraph $VERSION for $TARGET"
curl -fsSL -o "$TMP/SHA256SUMS" "$BASE/SHA256SUMS" || fail "release v$VERSION not found"
get "$PACKAGE.tar.gz"
check "$PACKAGE.tar.gz"
MODEL_ARCHIVE=mygraph-model-$MODEL.tar.gz
if [ -z "$NO_MODEL" ] && [ ! -f "$BIN/models/$MODEL/model.onnx" ]; then
  get "$MODEL_ARCHIVE"
  check "$MODEL_ARCHIVE"
fi

# --- install -----------------------------------------------------------------------------------

mkdir -p "$BIN" "$HOME_DIR/data" "$HOME_DIR/logs" "$LINK_DIR"
tar -xzf "$TMP/$PACKAGE.tar.gz" -C "$TMP"
stop_service
# Replace by rename, so MCP bridges still running the old binary keep working.
cp "$TMP/$PACKAGE/mygraph" "$EXE.new"
chmod 755 "$EXE.new"
mv -f "$EXE.new" "$EXE"
if [ "$OS" = Darwin ]; then xattr -d com.apple.quarantine "$EXE" 2>/dev/null || true; fi
rm -rf "$HOME_DIR/skills"
cp -R "$TMP/$PACKAGE/skills" "$HOME_DIR/skills"
cp "$TMP/$PACKAGE/README.md" "$TMP/$PACKAGE/SECURITY.md" "$TMP/$PACKAGE"/LICENSE-* "$HOME_DIR/"
if [ -f "$TMP/$MODEL_ARCHIVE" ]; then
  rm -rf "$BIN/models/$MODEL"
  tar -xzf "$TMP/$MODEL_ARCHIVE" -C "$BIN"
fi
ln -sf "$EXE" "$LINK_DIR/mygraph"

if [ ! -f "$CONFIG" ]; then
  cat > "$CONFIG" <<EOF
# MyGraph server configuration, written by install.sh; upgrades keep it.
# All settings: https://github.com/$REPO/blob/main/spec/api/conventions.md
# Without [[keys]] the server accepts only requests from this machine, as an admin.
listen = "127.0.0.1:$PORT"
data_dir = "$HOME_DIR/data"

[search]
model_dir = "$BIN/models/$MODEL"
EOF
fi
PORT=$(sed -n 's/^listen = "[^"]*:\([0-9]*\)"/\1/p' "$CONFIG" | head -n 1)
PORT=${PORT:-7420}

# --- service -----------------------------------------------------------------------------------

started=
if [ -z "$NO_SERVICE" ]; then
  case $OS in
    Linux)
      if has_systemd; then
        unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
        mkdir -p "$unit_dir"
        cat > "$unit_dir/$UNIT" <<EOF
[Unit]
Description=MyGraph server
After=network.target

[Service]
ExecStart="$EXE" --config "$CONFIG" serve
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF
        systemctl --user daemon-reload
        systemctl --user enable "$UNIT" >/dev/null 2>&1
        systemctl --user restart "$UNIT"
        started=systemd
      else
        say "no systemd user session here; start the server yourself (below)"
      fi ;;
    Darwin)
      mkdir -p "$(dirname "$PLIST")"
      cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$AGENT</string>
  <key>ProgramArguments</key>
  <array><string>$EXE</string><string>--config</string><string>$CONFIG</string><string>serve</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>StandardOutPath</key><string>$HOME_DIR/logs/server.log</string>
  <key>StandardErrorPath</key><string>$HOME_DIR/logs/server.log</string>
</dict>
</plist>
EOF
      launchctl bootstrap "gui/$(id -u)" "$PLIST"
      started=launchd ;;
  esac
fi

if [ -n "$started" ]; then
  ok=
  for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30; do
    if "$EXE" health --url "http://127.0.0.1:$PORT" >/dev/null 2>&1; then ok=1; break; fi
    sleep 1
  done
  if [ -n "$ok" ]; then
    say "MyGraph $VERSION is running at http://127.0.0.1:$PORT (started at login by $started)."
  else
    case $started in
      systemd) say "The server did not answer yet; see: journalctl --user -u $UNIT" ;;
      launchd) say "The server did not answer yet; see: $HOME_DIR/logs/server.log" ;;
    esac
  fi
else
  say "Installed MyGraph $VERSION. Start it with:"
  say "  mygraph --config \"$CONFIG\" serve"
fi

case ":$PATH:" in *":$LINK_DIR:"*) ;; *) say "Add $LINK_DIR to your PATH to run \`mygraph\` by name." ;; esac
cat <<EOF

Connect an agent:
  Claude Code:  claude plugin marketplace add $REPO && claude plugin install mygraph@mygraph
     or:        claude mcp add --transport http -s user mygraph http://127.0.0.1:$PORT/mcp
  Others:       https://github.com/$REPO#connecting-a-client
Then ask it to remember something. Program, data and settings: $HOME_DIR
EOF
