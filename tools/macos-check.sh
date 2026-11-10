#!/bin/bash
# Type-check the workspace for macOS (x86_64 and arm64) from Linux, see docs/platforms.md.
# ring's build script compiles C for the target, so this uses `zig cc` (it ships macOS libc
# headers) as the target C compiler. Needs zig 0.14 (ZIG=/path/to/zig, default `zig`) and
# `rustup target add x86_64-apple-darwin aarch64-apple-darwin`. Does not link (no macOS SDK).
# Extra arguments go to cargo check, e.g. `tools/macos-check.sh -j 4`.
set -euo pipefail
cd "$(dirname "$0")/.."
ZIG=${ZIG:-zig}
TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}
wrap="$TARGET_DIR/zigcc"
mkdir -p "$wrap"
cat > "$wrap/zig-ar" <<EOF
#!/bin/bash
export ZIG_GLOBAL_CACHE_DIR="$TARGET_DIR/zig-cache" ZIG_LOCAL_CACHE_DIR="$TARGET_DIR/zig-cache"
exec "$ZIG" ar "\$@"
EOF
for pair in "x86_64-apple-darwin x86_64-macos" "aarch64-apple-darwin aarch64-macos"; do
  read -r rust_target zig_target <<<"$pair"
  # drop the -arch / --target= / -mmacosx-version-min= flags cc-rs adds for Apple targets
  cat > "$wrap/$rust_target-cc" <<EOF
#!/bin/bash
export ZIG_GLOBAL_CACHE_DIR="$TARGET_DIR/zig-cache" ZIG_LOCAL_CACHE_DIR="$TARGET_DIR/zig-cache"
args=(); skip=0
for a in "\$@"; do
  if [ \$skip = 1 ]; then skip=0; continue; fi
  case "\$a" in -arch) skip=1 ;; --target=*|-mmacosx-version-min=*) ;; *) args+=("\$a") ;; esac
done
exec "$ZIG" cc -target $zig_target "\${args[@]}"
EOF
done
chmod +x "$wrap"/*
for t in x86_64-apple-darwin aarch64-apple-darwin; do
  u=${t//-/_}
  echo "== $t"
  env "CC_$u=$wrap/$t-cc" "AR_$u=$wrap/zig-ar" cargo check --target "$t" --workspace --all-targets "$@"
done
