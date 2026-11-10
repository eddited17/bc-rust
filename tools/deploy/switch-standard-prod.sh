#!/usr/bin/env bash
# switch-standard-prod.sh: move the live services to the `standard-prod` app set (docs/standard-apps.md,
# "Switching to standard-prod"), or back.
#
#   tools/deploy/switch-standard-prod.sh [--print]                       switch
#   tools/deploy/switch-standard-prod.sh [--print] --rollback [DIR]      back to the default apps
#   tools/deploy/switch-standard-prod.sh [--print] --rollback [DIR] --restore-data
#                                                                        ... and restore both databases
#
# Switch:
#   1. checks that both binaries know app sets and that the unit files have the expected ExecStart lines;
#   2. backs up both unit files and dumps databases `bc` and `bc_nst` (schemas c_cronus and shared,
#      pg_dump -Fc) into $BACKUP_ROOT/standard-prod-<UTC time>/ (symlink `latest`);
#   3. appends `--apps standard-prod` to the ExecStart lines of bc-rust.service and bc-nst.service;
#   4. systemctl --user daemon-reload, then restarts bc-rust.service, waits until it listens (its first
#      boot installs the set's apps), then bc-nst.service, then msweb-bcnst-8193.service (the web server
#      caches metadata per bc-nst start). Don't start both servers at once on a database that still
#      needs installs: both would insert the same "NAV App Installed App" rows.
# Rollback: puts the backed-up unit files back (from DIR, default the `latest` backup), daemon-reload,
#   restarts in the same order. The default apps run fine on a database that has the set's tables and
#   rows; the apps stay recorded as installed, so a later switch installs nothing. --restore-data also
#   restores both databases from the dumps (stop unit, drop its tables and schemas, pg_restore, start);
#   whatever was written after the switch is lost.
#
# --print: print every command (and the ExecStart lines before/after) without running anything.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# the binaries the units run (override only to check another build with --print)
BIN_DIR=${BIN_DIR:-$REPO/target/release}
UNIT_DIR=$HOME/.config/systemd/user
# where unit-file backups and database dumps go (required)
BACKUP_ROOT=${BACKUP_ROOT:-}
SET=standard-prod
PG_CONTAINER=bc-rust-postgres
WAIT_SECS=${WAIT_SECS:-180}
# unit, database, binary
SERVERS=("bc-rust.service bc bc" "bc-nst.service bc_nst bc-nst")
WEB_UNIT=msweb-bcnst-8193.service

PRINT=0 ROLLBACK=0 RESTORE_DATA=0 DIR=""
while [ $# -gt 0 ]; do
  case "$1" in
    --print) PRINT=1 ;;
    --rollback) ROLLBACK=1; if [ $# -gt 1 ] && [ "${2#--}" = "$2" ]; then DIR=$2; shift; fi ;;
    --restore-data) RESTORE_DATA=1 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
  shift
done
[ $RESTORE_DATA = 1 ] && [ $ROLLBACK = 0 ] && { echo "--restore-data goes with --rollback" >&2; exit 2; }
[ -n "$BACKUP_ROOT" ] || { echo "set BACKUP_ROOT, the directory for unit-file backups and database dumps" >&2; exit 2; }

say() { echo "== $*"; }
# run CMD...: run it, or print it with --print
run() {
  if [ $PRINT = 1 ]; then printf '+ %s\n' "$*"; else eval "$@"; fi
}
psql_db() { docker exec -i "$PG_CONTAINER" psql -U bc -d "$1" -v ON_ERROR_STOP=1 "${@:2}"; }

# the ExecStart line of a unit file, with or without the app set
exec_line() { grep -m1 '^ExecStart=' "$UNIT_DIR/$1"; }
with_set() { local l; l=$(exec_line "$1"); case "$l" in *"--apps "*) echo "$l" ;; *) echo "$l --apps $SET" ;; esac; }

# wait until UNIT logs "listening" after epoch SINCE; print its app-set lines
wait_listening() {
  local unit=$1 since=$2 i
  if [ $PRINT = 1 ]; then printf '+ wait until journalctl --user -u %s logs "listening" (up to %s s)\n' "$unit" "$WAIT_SECS"; return; fi
  for ((i = 0; i < WAIT_SECS; i++)); do
    if journalctl --user -u "$unit" --since "@$since" -o cat 2>/dev/null | grep -q "listening"; then
      journalctl --user -u "$unit" --since "@$since" -o cat | grep -E "app set|apps:|NOT INSTALLED|install failed|upgraded:|listening" | cut -c1-240
      return 0
    fi
    sleep 1
  done
  echo "$unit did not log 'listening' within $WAIT_SECS s; journal:" >&2
  journalctl --user -u "$unit" --since "@$since" -o cat | tail -20 >&2
  return 1
}

restart_in_order() {
  local entry unit t
  for entry in "${SERVERS[@]}"; do
    read -r unit _ _ <<< "$entry"
    t=$(date +%s)
    run "systemctl --user restart $unit"
    wait_listening "$unit" "$t"
  done
  run "systemctl --user restart $WEB_UNIT"
}

check_binary() {
  local bin=$BIN_DIR/$1
  [ -x "$bin" ] || { echo "missing $bin" >&2; exit 1; }
  # the built-in manifests are compiled in (bc_server::appsets)
  if ! grep -q "App set \"standard-prod\"" "$bin"; then
    echo "$bin predates app sets (P12): build it from a tree with crates/bc-server/app-sets first" >&2
    [ $PRINT = 1 ] && echo "  (--print: continuing; a real run stops here)" >&2 || exit 1
  fi
}

dump_db() {
  local db=$1 out=$2 f
  f=$db-$(basename "$out").dump
  run "docker exec $PG_CONTAINER pg_dump -U bc -d $db -n c_cronus -n shared -Fc -f /tmp/$f"
  run "docker cp $PG_CONTAINER:/tmp/$f $out/$db.dump"
  run "docker exec $PG_CONTAINER rm /tmp/$f"
}

restore_db() {
  local unit=$1 db=$2 dump=$3
  [ $PRINT = 1 ] || [ -s "$dump" ] || { echo "no dump $dump" >&2; exit 1; }
  run "systemctl --user stop $unit"
  # one DROP per table: a single DROP SCHEMA ... CASCADE runs out of shared memory (~1,900 tables)
  run "psql_db $db -Atc \"select format('DROP TABLE %I.%I;', schemaname, tablename) from pg_tables where schemaname in ('c_cronus','shared')\" | psql_db $db -q"
  run "psql_db $db -c 'DROP SCHEMA IF EXISTS c_cronus CASCADE; DROP SCHEMA IF EXISTS shared CASCADE;'"
  run "docker cp $dump $PG_CONTAINER:/tmp/restore-$db.dump"
  run "docker exec $PG_CONTAINER pg_restore -U bc -d $db /tmp/restore-$db.dump"
  run "docker exec $PG_CONTAINER rm /tmp/restore-$db.dump"
}

if [ $ROLLBACK = 0 ]; then
  ts=$(date -u +%Y%m%d-%H%M%S)
  out=$BACKUP_ROOT/standard-prod-$ts
  say "switch to --apps $SET $( [ $PRINT = 1 ] && echo '(--print: nothing is changed)')"
  for entry in "${SERVERS[@]}"; do
    read -r unit db bin <<< "$entry"
    [ -f "$UNIT_DIR/$unit" ] || { echo "missing $UNIT_DIR/$unit" >&2; exit 1; }
    case "$(exec_line "$unit")" in
      *"/target/release/$bin "*) ;;
      *) echo "$unit: unexpected ExecStart: $(exec_line "$unit")" >&2; exit 1 ;;
    esac
    check_binary "$bin"
    echo "$unit (database $db)"
    echo "  before: $(exec_line "$unit")"
    echo "  after:  $(with_set "$unit")"
  done
  say "backup to $out"
  run "mkdir -p $out"
  for entry in "${SERVERS[@]}"; do
    read -r unit db _ <<< "$entry"
    run "cp -p $UNIT_DIR/$unit $out/$unit"
    dump_db "$db" "$out"
  done
  run "ln -sfn $out $BACKUP_ROOT/latest"
  say "edit ExecStart lines"
  for entry in "${SERVERS[@]}"; do
    read -r unit _ _ <<< "$entry"
    if exec_line "$unit" | grep -q -- "--apps "; then
      echo "$unit already has --apps; left as it is"
    else
      run "sed -i '/^ExecStart=/ s|\$| --apps $SET|' $UNIT_DIR/$unit"
    fi
  done
  run "systemctl --user daemon-reload"
  say "restart: bc-rust, then bc-nst, then the web server"
  restart_in_order
  run "curl -s http://127.0.0.1:8740/api/stats"
  echo
  say "done. Roll back with: $0 --rollback $out"
else
  [ -n "$DIR" ] || DIR=$(readlink -f "$BACKUP_ROOT/latest" 2>/dev/null || true)
  if [ -z "$DIR" ] || [ ! -d "$DIR" ]; then
    [ $PRINT = 1 ] || { echo "no backup directory (give one, or $BACKUP_ROOT/latest)" >&2; exit 1; }
    # --print before any switch: show the rollback against a placeholder backup
    DIR="$BACKUP_ROOT/standard-prod-<UTC time>"
  fi
  say "roll back from $DIR $( [ $PRINT = 1 ] && echo '(--print: nothing is changed)')"
  for entry in "${SERVERS[@]}"; do
    read -r unit db _ <<< "$entry"
    echo "$unit: ExecStart now:      $(exec_line "$unit")"
    if [ -f "$DIR/$unit" ]; then
      echo "$unit: ExecStart restored: $(grep -m1 '^ExecStart=' "$DIR/$unit")"
    else
      [ $PRINT = 1 ] || { echo "missing $DIR/$unit" >&2; exit 1; }
      echo "$unit: ExecStart restored: (the backed-up line, i.e. without --apps)"
    fi
    if [ $RESTORE_DATA = 1 ]; then
      [ $PRINT = 1 ] || [ -s "$DIR/$db.dump" ] || { echo "missing $DIR/$db.dump" >&2; exit 1; }
    fi
  done
  for entry in "${SERVERS[@]}"; do
    read -r unit _ _ <<< "$entry"
    run "cp -p $DIR/$unit $UNIT_DIR/$unit"
  done
  run "systemctl --user daemon-reload"
  if [ $RESTORE_DATA = 1 ]; then
    say "restore databases"
    for entry in "${SERVERS[@]}"; do
      read -r unit db _ <<< "$entry"
      restore_db "$unit" "$db" "$DIR/$db.dump"
    done
  fi
  say "restart: bc-rust, then bc-nst, then the web server"
  restart_in_order
  say "rolled back"
fi
