#!/usr/bin/env bash
# Source-policy guard (SOURCES.md). Reports findings; exits 0 unless --strict is given.
#
# Usage: tools/legal/guard.sh [--strict] [--range A..B] [--bcapps DIR] [--quiet]
#        tools/legal/guard.sh --print-pin      # the BCApps commit docs/sources.md is pinned to
#
# Checks (tracked and untracked, not ignored, files of the working tree):
#   words     names of .NET decompilers and disassemblers
#   paths     local paths of the tool output that SOURCES.md withdrew
#   names     Nav* and Microsoft.Dynamics.Nav.* names that no public source declares. The allowlist is
#             generated: DotNet assemblies and types declared in BCApps AL (--bcapps, default
#             vendor/bcapps) and platform/ AL, plus docs/wire-vocab.tsv when present
#   files     *.dll, *.pdb, *.resx and golden/ directories in the tree
#   ledger    [src:ID] tags without a row in docs/sources.md; bcapps: paths missing at the pinned
#             commit; tests/vectors/ files that are not in the tree; bad status values
#   commits   words, paths and names in the commit messages of --range A..B
# SOURCES.md and tools/legal/ are exempt from words and paths, platform/ (Microsoft's MIT AL) from names.
set -uo pipefail

STRICT=0 RANGE="" BCAPPS="" QUIET=0 PRINT_PIN=0
while [ $# -gt 0 ]; do
  case "$1" in
    --strict) STRICT=1 ;;
    --range) RANGE="${2:-}"; shift ;;
    --bcapps) BCAPPS="${2:-}"; shift ;;
    --quiet) QUIET=1 ;;
    --print-pin) PRINT_PIN=1 ;;
    -h|--help) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "guard: unknown argument $1" >&2; exit 2 ;;
  esac
  shift
done

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || { echo "guard: not in a git work tree" >&2; exit 2; }
cd "$ROOT" || exit 2
LEDGER=docs/sources.md
PIN=$(sed -n 's/^BCApps pin: `\([0-9a-f]\{40\}\)`.*/\1/p' "$LEDGER" 2>/dev/null | head -1)
if [ "$PRINT_PIN" = 1 ]; then
  [ -n "$PIN" ] || { echo "guard: no BCApps pin in $LEDGER" >&2; exit 2; }
  echo "$PIN"; exit 0
fi
[ -n "$BCAPPS" ] || BCAPPS=vendor/bcapps

FINDINGS=""
note() { [ "$QUIET" = 1 ] || echo "guard: $*" >&2; }
# add CHECK < lines: one finding per non-empty line
add() { local l; while IFS= read -r l; do [ -n "$l" ] && FINDINGS+="$1"$'\t'"${l:0:240}"$'\n'; done; return 0; }
# not_in LIST < file:line:token lines: keep the lines whose last field is not in LIST
not_in() { awk -F: 'NR == FNR { ok[$0] = 1; next } { if (!($NF in ok)) print }' <(printf '%s\n' "$1") -; }

WORDS='decompil|ilspy|dnspy|dotpeek'
PATHS='nst-decomp|odata-decomp|nua-decomp|p2-decomp|bc-artifacts/decomp|/tmp/bcproto|\bp(7|10|12)tmp\b|\bpenvtmp\b|perm-p13/(alc|pth)|i15/l/decomp|i15/o/answers\.md|quarantine/decompile'
NAMES='\bNav[A-Z]\w+|\bMicrosoft\.Dynamics\.Nav(?:\.\w+)+'
DECL='^\s*(assembly|type)\(\s*"\K[^"]+("\s*;\s*"[^"]+)?'
GG=(git grep --untracked -I -n)

# words, paths
add words < <("${GG[@]}" -i -E "$WORDS" -- . ':!SOURCES.md' ':!tools/legal')
add paths < <("${GG[@]}" -P "$PATHS" -- . ':!SOURCES.md' ':!tools/legal')

# names, against a generated allowlist
if [ ! -d "$BCAPPS/src" ]; then
  note "no BCApps at $BCAPPS: the names allowlist has no BCApps names (clone it, or pass --bcapps DIR)"
fi
[ -f docs/wire-vocab.tsv ] || note "docs/wire-vocab.tsv not found: no wire names in the allowlist"
ALLOW=$({
  [ -d "$BCAPPS/src" ] && grep -rhoP --include='*.al' "$DECL" "$BCAPPS/src"
  git grep -h -o -P "$DECL" -- 'platform/*.al'
  # Names Microsoft's public AL uses anywhere (AL types such as NavApp, setting values such as
  # NavUserPassword) are public too.
  [ -d "$BCAPPS/src" ] && grep -rhoP --include='*.al' "$NAMES" "$BCAPPS/src"
  git grep -h -o -P "$NAMES" -- 'platform/*.al'
  [ -f docs/wire-vocab.tsv ] && grep -oP "$NAMES" docs/wire-vocab.tsv
} 2>/dev/null | sed -E 's/"\s*;\s*"/\n/' |
  awk -F. '{ print; s = $1; for (i = 2; i < NF; i++) { s = s "." $i; if (i >= 4) print s } }' | sort -u)
note "names allowlist: $(printf '%s\n' "$ALLOW" | grep -c .) generated entries"
add names < <("${GG[@]}" -o -P "$NAMES" -- . ':!platform' ':!tools/legal' | not_in "$ALLOW" | sort -u)

# files
add files < <(git ls-files --cached --others --exclude-standard | grep -iE '\.(dll|pdb|resx)$|(^|/)golden/')

# ledger
if [ ! -f "$LEDGER" ]; then
  add ledger <<< "$LEDGER is missing"
else
  # rows: | ID | Area | Code | Tier | Reference | Status | Date |
  ROWS=$(awk -F'|' '/^\| *[A-Z][A-Za-z0-9._-]* *\|/ { id = $2; gsub(/ /, "", id); if (id == "ID") next;
                     st = $7; gsub(/ /, "", st); t = $5; gsub(/ /, "", t); r = $6; gsub(/ /, "", r);
                     print id "\t" st "\t" (t == "" ? "-" : "t") "\t" (r == "" ? "-" : "r") }' "$LEDGER")
  IDS=$(printf '%s\n' "$ROWS" | cut -f1 | sort -u)
  add ledger < <(printf '%s\n' "$ROWS" | cut -f1 | sort | uniq -d | sed "s|^|$LEDGER: duplicate ID |")
  add ledger < <(printf '%s\n' "$ROWS" | awk -F'\t' -v f="$LEDGER" '
    NF >= 2 && $2 != "open" && $2 != "sourced" && $2 != "removed" { print f ": " $1 ": status \"" $2 "\" is not open, sourced or removed" }
    $2 == "sourced" && ($3 == "-" || $4 == "-") { print f ": " $1 ": sourced without a tier and a reference" }')
  add ledger < <("${GG[@]}" -o -P '\[src:\K[A-Za-z0-9][A-Za-z0-9._-]*(?=\])' -- . ':!tools/legal' | not_in "$IDS" |
                 sed "s|\$| ([src:] tag without a row in $LEDGER)|")
  BCPATHS=$(grep -oP '`bcapps:\K[^`<][^`]*' "$LEDGER" | sort -u)
  if [ -n "$BCPATHS" ]; then
    if [ -n "$PIN" ] && git -C "$BCAPPS" cat-file -e "$PIN^{commit}" 2>/dev/null; then
      add ledger < <(while IFS= read -r p; do
        [ -n "$(git -C "$BCAPPS" ls-tree --name-only "$PIN" -- "$p")" ] || echo "$LEDGER: bcapps:$p is not in BCApps at $PIN"
      done <<< "$BCPATHS")
    else
      note "BCApps pin ${PIN:-(none)} is not in $BCAPPS: bcapps: paths not checked"
    fi
  fi
  add ledger < <(grep -oP 'tests/vectors/[A-Za-z0-9._/-]+\.tsv' "$LEDGER" | sort -u |
                 while IFS= read -r v; do [ -f "$v" ] || echo "$LEDGER: $v is not in the tree"; done)
fi

# commit messages
if [ -n "$RANGE" ]; then
  A=${RANGE%%..*}
  if [ "$A" = "$RANGE" ] || ! git cat-file -e "$A^{commit}" 2>/dev/null; then
    note "range $RANGE: base commit not available, commit messages not checked"
  else
    LOG=$(git log --format='%h %s%n%b' "$RANGE") || note "range $RANGE: git log failed"
    add commits < <(printf '%s\n' "$LOG" | grep -i -E "$WORDS" | sed 's/^/words: /')
    add commits < <(printf '%s\n' "$LOG" | grep -P "$PATHS" | sed 's/^/paths: /')
    add commits < <(printf '%s\n' "$LOG" | grep -n -o -P "$NAMES" | not_in "$ALLOW" | sed 's/^/names: message line /')
  fi
fi

N=$(printf '%s' "$FINDINGS" | grep -c .)
[ "$QUIET" = 1 ] || printf '%s' "$FINDINGS" | sed 's/\t/: /'
SUMMARY=$(printf '%s' "$FINDINGS" | cut -f1 | sort | uniq -c | awk '{ printf "%s%s %s", sep, $2, $1; sep = ", " }')
MODE=$([ "$STRICT" = 1 ] && echo strict || echo warn)
echo "guard: $N findings${SUMMARY:+ ($SUMMARY)}, $MODE mode"
if [ -n "${GITHUB_ACTIONS:-}" ] && [ "$N" -gt 0 ]; then
  echo "::warning title=source-policy guard::$N findings${SUMMARY:+ ($SUMMARY)}, see the job log"
fi
[ "$STRICT" = 1 ] && [ "$N" -gt 0 ] && exit 1
exit 0
