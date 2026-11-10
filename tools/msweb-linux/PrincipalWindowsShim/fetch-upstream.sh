#!/usr/bin/env bash
# Make the source of the System.Security.Principal.Windows shim from dotnet/runtime:
#   1. download the reference source, Strings.resx and the csproj from github.com/dotnet/runtime at the .NET
#      release the web server was built against, checked against upstream.lock;
#   2. turn the reference source into the Linux "not supported" implementation, as dotnet/runtime's build
#      does (gen-notsupported.py);
#   3. apply bc-linux.patch.
# The result goes to gen/ (git-ignored), which PrincipalWindowsShim.csproj compiles. run.sh --install calls this.
#
#   fetch-upstream.sh [<web server dir>]                  # tag from the server's deps.json, else the pinned tag
#   DOTNET_RUNTIME_TAG=v10.0.13 fetch-upstream.sh         # explicit tag
#   fetch-upstream.sh --update-lock [<web server dir>]    # pin a tag that upstream.lock doesn't list yet
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCK="$HERE/upstream.lock"
REPO=dotnet/runtime
DIR=src/libraries/System.Security.Principal.Windows
FILES=("$DIR/ref/System.Security.Principal.Windows.cs" "$DIR/src/Resources/Strings.resx"
       "$DIR/src/System.Security.Principal.Windows.csproj")

update_lock=0
if [ "${1:-}" = --update-lock ]; then update_lock=1; shift; fi
msweb="${1:-}"

# The release of the dotnet/runtime packages that deps.json lists. They must agree.
tag_from_deps() {
  python3 -I - "$1" <<'EOF'
import json, sys
from pathlib import Path
app = Path(sys.argv[1])
deps = app / "Prod.Client.WebCoreApp.deps.json.orig"
if not deps.exists():
    deps = app / "Prod.Client.WebCoreApp.deps.json"
libs = json.loads(deps.read_text(encoding="utf-8-sig"))["libraries"]
names = ("Microsoft.Bcl.AsyncInterfaces", "System.Configuration.ConfigurationManager",
         "System.Security.Cryptography.ProtectedData")
versions = {k.split("/")[1] for k in libs if k.split("/")[0] in names}
if len(versions) != 1:
    sys.exit(f"fetch-upstream: expected one version of {', '.join(names)} in {deps}, found {sorted(versions)}")
print("v" + versions.pop())
EOF
}

if [ -n "${DOTNET_RUNTIME_TAG:-}" ]; then
  tag="$DOTNET_RUNTIME_TAG"
elif [ -n "$msweb" ]; then
  tag="$(tag_from_deps "$msweb")"
else
  tag="$(awk -F'\t' '!/^#/ && NF == 4 { t = $1 } END { print t }' "$LOCK")"
fi

if ! grep -q "^$tag	" "$LOCK"; then
  [ "$update_lock" = 1 ] || {
    echo "fetch-upstream: $tag is not in upstream.lock; rerun with --update-lock to pin it" >&2
    exit 1
  }
  commit="$(git ls-remote "https://github.com/$REPO" "refs/tags/$tag" | cut -f1)"
  [ -n "$commit" ] || { echo "fetch-upstream: no tag $tag in $REPO" >&2; exit 1; }
  for path in "${FILES[@]}"; do
    sum="$(curl -fsSL "https://raw.githubusercontent.com/$REPO/$commit/$path" | sha256sum | cut -d' ' -f1)"
    printf '%s\t%s\t%s\t%s\n' "$tag" "$commit" "$sum" "$path" >>"$LOCK"
  done
  echo "fetch-upstream: pinned $tag ($commit) in upstream.lock"
fi

cache="$HERE/upstream/$tag"
mkdir -p "$cache"
while IFS=$'\t' read -r _ commit sum path; do
  file="$cache/$(basename "$path")"
  if [ ! -f "$file" ] || ! echo "$sum  $file" | sha256sum -c --quiet - 2>/dev/null; then
    curl -fsSL -o "$file.part" "https://raw.githubusercontent.com/$REPO/$commit/$path"
    echo "$sum  $file.part" | sha256sum -c --quiet - ||
      { echo "fetch-upstream: $path at $tag does not match upstream.lock" >&2; exit 1; }
    mv "$file.part" "$file"
  fi
done < <(awk -F'\t' -v t="$tag" '!/^#/ && $1 == t' "$LOCK")

# The exception message upstream's csproj names for non-Windows builds, e.g. SR.PlatformNotSupported_Principal.
message="$(sed -n 's|.*<GeneratePlatformNotSupportedAssemblyMessage[^>]*>\([^<]*\)<.*|\1|p' \
  "$cache/System.Security.Principal.Windows.csproj")"
[ -n "$message" ] || { echo "fetch-upstream: no GeneratePlatformNotSupportedAssemblyMessage upstream" >&2; exit 1; }

# The generated "not supported" source stays next to the downloads; gen/ gets only the patched result.
python3 -I "$HERE/gen-notsupported.py" "$cache/System.Security.Principal.Windows.cs" "$cache/Strings.resx" \
  "$message" "$cache/System.Security.Principal.Windows.notsupported.cs" "$cache/SR.cs"
mkdir -p "$HERE/gen"
patch --quiet --fuzz=0 --no-backup-if-mismatch -o "$HERE/gen/System.Security.Principal.Windows.cs.part" \
  "$cache/System.Security.Principal.Windows.notsupported.cs" "$HERE/bc-linux.patch"
mv "$HERE/gen/System.Security.Principal.Windows.cs.part" "$HERE/gen/System.Security.Principal.Windows.cs"
cp "$cache/SR.cs" "$HERE/gen/SR.cs"
echo "$tag" >"$HERE/gen/TAG"
echo "fetch-upstream: gen/ built from $REPO $tag with bc-linux.patch"
