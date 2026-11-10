#!/bin/bash
# tools/crashcap/test.sh: build crashcap.so and crashtest in a fresh temporary directory (never in the repo), crash
# crashtest three ways (heap corruption, null pointer, abort) with worker threads alive, and check that each run
# 1) dies with the same signal as without the library and 2) leaves a backtrace of every thread from gdb.
# Usage: tools/crashcap/test.sh [WORKDIR]   (WORKDIR must not exist; default: a new mktemp -d under $TMPDIR)
set -u
SRC=$(cd "$(dirname "$0")" && pwd)
W=${1:-$(mktemp -d)}
[ -e "$W" ] && [ -n "$(ls -A "$W")" ] && { echo "test.sh: $W is not empty" >&2; exit 2; }
mkdir -p "$W/out" || exit 2
command -v gdb >/dev/null || { echo "test.sh: gdb not installed" >&2; exit 2; }
gcc -O2 -fPIC -shared -Wall -Wextra -Werror -o "$W/crashcap.so" "$SRC/crashcap.c" || exit 1
gcc -O0 -g -pthread -w -o "$W/crashtest" "$SRC/crashtest.c" || exit 1
fail=0
for mode in heap segv abort; do
  "$W/crashtest" "$mode" 2>/dev/null; plain=$?
  before=$(ls "$W/out" | wc -l)
  LD_PRELOAD="$W/crashcap.so" CRASHCAP_DIR="$W/out" "$W/crashtest" "$mode" 2> "$W/$mode.err"; rc=$?
  f=$(ls -t "$W/out" | head -1)
  n=$(( $(ls "$W/out" | wc -l) - before ))
  threads=$(grep -c '^Thread [0-9]' "$W/out/$f" 2>/dev/null)
  ok=1
  [ "$rc" = "$plain" ] && [ "$rc" -gt 128 ] || ok=0
  [ "$n" = 1 ] && grep -q 'signal handler called' "$W/out/$f" && [ "$threads" -ge 4 ] || ok=0
  grep -q '^crashcap: SIG' "$W/$mode.err" || ok=0
  echo "$mode: exit $rc (without the library $plain), capture $f, $threads threads: $([ $ok = 1 ] && echo ok || echo FAILED)"
  [ $ok = 1 ] || fail=1
done
# the library must not reach child processes
child=$(LD_PRELOAD="$W/crashcap.so" sh -c 'echo "[$LD_PRELOAD]"')
[ "$child" = "[]" ] && echo "child environment: LD_PRELOAD removed: ok" || { echo "child environment: LD_PRELOAD=$child: FAILED"; fail=1; }
# CRASHCAP_OFF=1 installs nothing: no capture file
before=$(ls "$W/out" | wc -l)
CRASHCAP_OFF=1 LD_PRELOAD="$W/crashcap.so" CRASHCAP_DIR="$W/out" "$W/crashtest" abort 2>/dev/null; rc=$?
[ "$rc" = 134 ] && [ "$(ls "$W/out" | wc -l)" = "$before" ] && echo "CRASHCAP_OFF=1: exit $rc, no capture: ok" || { echo "CRASHCAP_OFF=1: FAILED"; fail=1; }
echo "workdir $W"
[ $fail = 0 ] && echo PASS || echo FAIL
exit $fail
