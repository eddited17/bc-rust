#!/bin/bash
# quick.sh DATASET OUT [AL_TEST]: the ratchet's quick slice (data/parity/ratchet/slice-DATASET.txt), a
# representative set of codeunits that runs in under 10 minutes at --jobs 4, then the ratchet check of it.
# DATASET: w1demo (suite baseapp) or contoso (suites system,foundation,baseapp). Run it from the root of the tree
# whose al-test you test (the tree's vendor/ and platform/ are read; the demo company cache is data/parity/cache).
# AL_TEST defaults to target/release/al-test (or $CARGO_TARGET_DIR/release/al-test). Exit status: the ratchet's
# (0 ok, 1 a must-pass test was lost, 2 the run is void).
set -u
DS=${1:?dataset: w1demo or contoso}
OUT=${2:?output dir}
HERE=$(cd "$(dirname "$0")" && pwd)
BIN=${3:-${CARGO_TARGET_DIR:-target}/release/al-test}
case $DS in
  w1demo) SUITE=baseapp ;;
  contoso) SUITE=system,foundation,baseapp ;;
  *) echo "quick.sh: unknown dataset $DS" >&2; exit 2 ;;
esac
CUS=$(python3 "$HERE/ratchet.py" slice "$DS") || exit 2
LABEL=quick-$DS-$(date -u +%H%M%S)
mkdir -p "$OUT"
echo "quick slice $DS: $(echo "$CUS" | tr ',' '\n' | wc -l) codeunits, bin $BIN $(md5sum < "$BIN" | cut -c1-32)"
"$BIN" --suite $SUITE --data $DS --codeunit "$CUS" --jobs 4 --include-disabled -v --label "$LABEL" --out "$OUT" > "$OUT/$LABEL.out" 2> "$OUT/$LABEL.log"
echo "al-test exit $?; $(grep -m1 '^passed:' "$OUT/$LABEL.out")"
python3 "$HERE/ratchet.py" check "$OUT"/*"-$LABEL-results.jsonl"
