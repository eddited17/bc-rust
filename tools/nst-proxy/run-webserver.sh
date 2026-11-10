#!/usr/bin/env bash
# Run a copy of Microsoft's BC web server (see tools/msweb-linux/) that talks to the NST through
# tools/nst-proxy/proxy.mjs. Same environment as tools/msweb-linux/run.sh, but it never builds or
# installs anything: the copy is made once from the already-patched server folder.
#
#   tools/nst-proxy/run-webserver.sh            # foreground on 127.0.0.1:8192
#   tools/nst-proxy/run-webserver.sh --copy     # (re)create the copy from webserver-linux, then exit
#
# Env: PROXY_WEB_DIR (required), SRC_WEB_DIR (default MSWEB_DIR, the copy tools/msweb-linux/run.sh uses),
#      PROXY_WEB_PORT (default 8192), NST_PROXY_PORT (default 7285), BC_LINUX_LOG_LEVEL (default Warning).
# Machine-local defaults live outside the repository, in ${XDG_CONFIG_HOME:-~/.config}/bc-rust/msweb.env, as
# KEY=value lines: MSWEB_DIR (the web server copy) and PROXY_WEB_DIR (tools/nst-proxy's copy). The
# environment wins over the file.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONF="${XDG_CONFIG_HOME:-$HOME/.config}/bc-rust/msweb.env"
if [ -f "$CONF" ]; then
  while IFS='=' read -r key value; do
    case "$key" in MSWEB_DIR|PROXY_WEB_DIR) [ -n "${!key:-}" ] || printf -v "$key" '%s' "$value" ;; esac
  done <"$CONF"
fi
DIR="${PROXY_WEB_DIR:?set PROXY_WEB_DIR to the proxy copy of the web server, or add PROXY_WEB_DIR=<dir> to $CONF}"
SRC="${SRC_WEB_DIR:-${MSWEB_DIR:-}}"
PORT="${PROXY_WEB_PORT:-8192}"
NST_PORT="${NST_PROXY_PORT:-7285}"
export DOTNET_ROOT="${DOTNET_ROOT:-$HOME/.dotnet}"

if [ "${1:-}" = "--copy" ]; then
  [ -n "$SRC" ] && [ -d "$SRC" ] || { echo "no source copy '$SRC' (set SRC_WEB_DIR or MSWEB_DIR)" >&2; exit 1; }
  rm -rf "$DIR"
  cp -a "$SRC" "$DIR"   # -a keeps the hard-linked and symlinked case aliases
  # the only change: the web server dials the proxy instead of the NST
  sed -i -E "s/(\"ClientServicesPort\": *)\"?[0-9]+\"?/\1$NST_PORT/" "$DIR/navsettings.json"
  # a private copy of the applicationHost.config stand-in (keeps the /BC casing)
  cp "$HERE/../msweb-linux/applicationHost.config" "$DIR/applicationHost.config"
  grep -n '"ClientServicesPort"' "$DIR/navsettings.json"
  exit 0
fi

[ -f "$DIR/BcLinux.HostingStartup.dll" ] || { echo "run $0 --copy first" >&2; exit 1; }
cd "$DIR"
export ASPNETCORE_APPL_PATH=/BC
export APP_POOL_CONFIG="$DIR/applicationHost.config"
export APP_POOL_ID=BC
export ASPNETCORE_HOSTINGSTARTUPASSEMBLIES=BcLinux.HostingStartup
export ASPNETCORE_URLS="http://127.0.0.1:$PORT"
export NAVWebSettings__RequireSsl=false
export BC_LINUX_LOG_LEVEL="${BC_LINUX_LOG_LEVEL:-Warning}"
# Extra variables the msweb-linux fixes may need, e.g. EXTRA_ENV="A=1 B=2".
for kv in ${EXTRA_ENV:-}; do export "$kv"; done
exec "$DOTNET_ROOT/dotnet" Prod.Client.WebCoreApp.dll
