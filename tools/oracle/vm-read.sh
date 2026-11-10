#!/usr/bin/env bash
# Ledger oracle: run read-sql.ps1 inside the Business Central container of the bc-sidebyside VM and save its JSON.
#   tools/oracle/vm-read.sh probe|baseline|setup OUT.json
#   tools/oracle/vm-read.sh read OUT.json BASE.json      (BASE.json = the baseline file from the same side)
# Env: ORACLE_VM_KEY (default data/vm/bc-vm_ed25519), ORACLE_VM_SSH (default bcadmin@127.0.0.1),
#      ORACLE_VM_PORT (default 2222), ORACLE_CONTAINER (default bcside). Read-only on the BC side.
set -euo pipefail
MODE=${1:?probe|baseline|setup|read}; OUT=${2:?out.json}; BASE=${3:-}
HERE=$(cd "$(dirname "$0")" && pwd)
KEY=${ORACLE_VM_KEY:-data/vm/bc-vm_ed25519}
PRE="\$Mode='$MODE';"
[ -n "${ORACLE_COMPANY:-}" ] && PRE="$PRE\$Company='$ORACLE_COMPANY';"
# SQL login instead of Windows auth, only if set (ORACLE_SQL_USER, ORACLE_SQL_PASSWORD_FILE)
[ -n "${ORACLE_SQL_PASSWORD_FILE:-}" ] && PRE="$PRE\$SqlUser='${ORACLE_SQL_USER:-sa}';\$SqlPassword='$(cat "$ORACLE_SQL_PASSWORD_FILE")';"
if [ "$MODE" = read ]; then
  [ -n "$BASE" ] || { echo "read needs BASE.json" >&2; exit 2; }
  # only the entryNo object, compact, single-quoted for PowerShell
  B=$(node -e 'const b=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")); process.stdout.write(JSON.stringify({entryNo:b.entryNo}))' "$BASE")
  PRE="$PRE\$BaseJson='$B';"
fi
if [ "$MODE" = setup ]; then
  S=$(node "$HERE/setup.mjs" spec)
  PRE="$PRE\$SetupJson='$S';"
fi
SSHO=(-i "$KEY" -o BatchMode=yes -o ConnectTimeout=20 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR)
VM=${ORACLE_VM_SSH:-bcadmin@127.0.0.1}; C=${ORACLE_CONTAINER:-bcside}
# copy the script to BcContainerHelper's folder, which the container mounts at the same path (a running
# Hyper-V container refuses docker cp, and a command line can't carry the script), then run a small bootstrap
SHARED=${ORACLE_SHARED:-C:/ProgramData/BcContainerHelper}
scp -q "${SSHO[@]}" -P "${ORACLE_VM_PORT:-2222}" "$HERE/read-sql.ps1" "$VM:$SHARED/oracle-read-sql.ps1"
ENC=$(printf "%s iex (Get-Content -Raw '%s/oracle-read-sql.ps1')\n" "$PRE" "$SHARED" | iconv -f UTF-8 -t UTF-16LE | base64 -w0)
ssh "${SSHO[@]}" -p "${ORACLE_VM_PORT:-2222}" "$VM" "docker exec $C powershell -NoProfile -NonInteractive -EncodedCommand $ENC" > "$OUT.tmp"
# PowerShell may wrap stdout in CRLFs; the JSON is one line
tr -d '\r' < "$OUT.tmp" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const j=JSON.parse(s.trim());require("fs").writeFileSync(process.argv[1],JSON.stringify(j));const t=j.tables?Object.entries(j.tables).map(([k,v])=>k+":"+(v.rows||[]).length).join(" "):"";console.log("wrote",process.argv[1],j.database,j.companyPrefix,t,j.entryNo?JSON.stringify(j.entryNo):"")})' "$OUT"
mv "$OUT.tmp" "$OUT.raw"
