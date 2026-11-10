#!/usr/bin/env bash
# Start Microsoft's BC web server (Prod.Client.WebCoreApp, ASP.NET Core / .NET 10) natively on Linux.
# See README.md in this folder for every change made to the server copy and why.
#
#   tools/msweb-linux/run.sh            # foreground, logs to stdout
#   tools/msweb-linux/run.sh --install  # (re)build the shims, copy them into the server, patch deps.json, exit
#
# Environment: MSWEB_DIR (required), MSWEB_PORT (default 8190), BC_LINUX_LOG_LEVEL (default Warning).
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
MSWEB_DIR="${MSWEB_DIR:?set MSWEB_DIR to your copy of the web server, or add MSWEB_DIR=<dir> to $CONF}"
MSWEB_PORT="${MSWEB_PORT:-8190}"
export DOTNET_ROOT="${DOTNET_ROOT:-$HOME/.dotnet}"
DOTNET="$DOTNET_ROOT/dotnet"

install_shims() {
  export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1
  # The Principal shim is built from dotnet/runtime source at the release this server's deps.json names.
  "$HERE/PrincipalWindowsShim/fetch-upstream.sh" "$MSWEB_DIR"
  for proj in EventLogStub PrincipalWindowsShim LinuxHostingStartup; do
    "$DOTNET" build -c Release -o "$HERE/$proj/out" "$HERE/$proj" >/dev/null
  done
  local shim
  for shim in Kernel32Shim/kernel32 User32Shim/user32 GdiPlusShim/gdiplus; do
    mkdir -p "$HERE/${shim%/*}/out"
    gcc -shared -fPIC -O2 -Wall -o "$HERE/${shim%/*}/out/libbclinux-${shim#*/}.so" "$HERE/${shim}shim.c"
  done
  cp "$HERE/EventLogStub/out/Microsoft.Extensions.Logging.EventLog.dll" \
     "$HERE/PrincipalWindowsShim/out/System.Security.Principal.Windows.dll" \
     "$HERE/LinuxHostingStartup/out/BcLinux.HostingStartup.dll" \
     "$HERE/Kernel32Shim/out/libbclinux-kernel32.so" \
     "$HERE/User32Shim/out/libbclinux-user32.so" \
     "$HERE/GdiPlusShim/out/libbclinux-gdiplus.so" "$MSWEB_DIR/"
  [ -f "$MSWEB_DIR/Prod.Client.WebCoreApp.deps.json.orig" ] ||
    cp "$MSWEB_DIR/Prod.Client.WebCoreApp.deps.json" "$MSWEB_DIR/Prod.Client.WebCoreApp.deps.json.orig"
  python3 -I "$HERE/patch-deps.py" "$MSWEB_DIR"
  # Folders the Windows installer creates; ResourcePersistenceManager refuses to start without them.
  mkdir -p "$MSWEB_DIR/wwwroot/Resources/ExtractedResources" "$MSWEB_DIR/wwwroot/Thumbnails"
  # Case-sensitive file system: the code and JS use names whose case differs from the files on disk
  # (Windows does not care). Add aliases with the expected case, next to the real file or folder:
  # a symlink for folders, a hard link (or copy) for files. Do not symlink files: ASP.NET Core's static
  # file middleware sends the symlink's own size as Content-Length (7 bytes for "Boot.js").
  # Where these come from: see README.md [src:TODO].
  local w="$MSWEB_DIR/wwwroot" alias target ext dir
  while read -r alias target; do
    dir="$w/$(dirname "$alias")"
    for ext in "" .br .gz; do
      [ -e "$dir/$target$ext" ] || continue
      [ -L "$w/$alias$ext" ] && [ -f "$dir/$target$ext" ] && rm "$w/$alias$ext"   # replace old file symlinks
      [ -e "$w/$alias$ext" ] && continue
      if [ -d "$dir/$target$ext" ]; then ln -s "$target$ext" "$w/$alias$ext"
      else ln "$dir/$target$ext" "$w/$alias$ext" 2>/dev/null || cp -p "$dir/$target$ext" "$w/$alias$ext"; fi
    done
  done <<'EOF'
Resources/Brand brand
Resources/Images images
Resources/Fonts fonts
js/boot.js Boot.js
js/OfficeClientAddIn.js OfficeClientAddin.js
js/file-saver-1.3.3/filesaver.min.js FileSaver.min.js
EOF
}

if [ "${1:-}" = "--install" ]; then
  install_shims
  exit 0
fi
[ -f "$MSWEB_DIR/BcLinux.HostingStartup.dll" ] || install_shims

cd "$MSWEB_DIR"   # the app resolves hosting.json, wwwroot and navsettings.json from the current directory

# Any ASPNETCORE_APPL_PATH selects Program.Main's "inside IIS" branch: Kestrel instead of HttpSys (Windows only).
export ASPNETCORE_APPL_PATH=/BC
# Keep the "/BC" casing instead of the lowercased "bc" (see applicationHost.config).
export APP_POOL_CONFIG="$HERE/applicationHost.config"
export APP_POOL_ID=BC
# Sets Request.PathBase to /BC (the job IIS does on Windows) and adds a console logger.
export ASPNETCORE_HOSTINGSTARTUPASSEMBLIES=BcLinux.HostingStartup
# Kestrel endpoint. Loopback only.
export ASPNETCORE_URLS="http://127.0.0.1:$MSWEB_PORT"
# Plain HTTP: secure-only antiforgery/auth cookies would make the sign-in page fail with HTTP 500.
# Set back to true when serving through an HTTPS reverse proxy.
export NAVWebSettings__RequireSsl=false
export BC_LINUX_LOG_LEVEL="${BC_LINUX_LOG_LEVEL:-Warning}"

exec "$DOTNET" Prod.Client.WebCoreApp.dll
