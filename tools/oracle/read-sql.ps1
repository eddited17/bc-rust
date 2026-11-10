# Ledger oracle, Business Central side: read the ledger tables straight from the container's SQL Server
# (read-only SELECTs). Runs INSIDE the BC container (Windows PowerShell 5.1), normally through
# tools/oracle/vm-read.sh, which prepends the parameters:
#   $Mode = 'baseline' | 'read' | 'probe' | 'setup'
#   $SetupJson = '[{"id":"98","name":"General Ledger Setup","wc":"","wv":[]},...]'   (setup mode, from setup.mjs spec)
#   $BaseJson = '{"entryNo":{"17":123,...}}'   (read mode: the baseline)
#   optional $SqlServer (default localhost\SQLEXPRESS), $SqlUser/$SqlPassword (default: Windows auth), $Database (default: the one holding the Base App tables),
#   $Company (default: the company whose G/L Entry table exists; the first if several)
# Output: one ASCII JSON document on stdout (non-ASCII characters escaped as \uXXXX).
$ErrorActionPreference = 'Stop'
if (-not $SqlServer) { $SqlServer = 'localhost\SQLEXPRESS' }
$BaseAppId = '437dbf0e-84ff-417a-965d-ed2bb9650972'
$Tables = [ordered]@{ '17' = 'G/L Entry'; '254' = 'VAT Entry'; '21' = 'Cust. Ledger Entry'; '379' = 'Detailed Cust. Ledg. Entry';
  '25' = 'Vendor Ledger Entry'; '380' = 'Detailed Vendor Ledg. Entry'; '32' = 'Item Ledger Entry'; '5802' = 'Value Entry' }
function SqlName([string]$s) { return ($s -replace '[./\\''"%\[\]]', '_') }

function Open-Db([string]$db) {
  $auth = if ($SqlPassword) { "User ID=$SqlUser;Password=$SqlPassword" } else { 'Integrated Security=true' }
  $c = New-Object System.Data.SqlClient.SqlConnection("Server=$SqlServer;Database=$db;$auth;Connect Timeout=15")
  $c.Open(); return $c
}
function Query($conn, [string]$sql) {
  $cmd = $conn.CreateCommand(); $cmd.CommandText = $sql; $cmd.CommandTimeout = 120
  $r = $cmd.ExecuteReader(); $rows = New-Object System.Collections.ArrayList
  $inv = [System.Globalization.CultureInfo]::InvariantCulture
  while ($r.Read()) {
    $o = [ordered]@{}
    for ($i = 0; $i -lt $r.FieldCount; $i++) {
      $n = $r.GetName($i); $v = $r.GetValue($i)
      if ($v -is [System.DBNull]) { $s = '' }
      elseif ($v -is [datetime]) { $s = $v.ToString('yyyy-MM-dd HH:mm:ss.fff', $inv) }
      elseif ($v -is [decimal]) { $s = $v.ToString($inv) }
      elseif ($v -is [byte[]]) { $s = '<bin>' }
      elseif ($v -is [bool]) { $s = if ($v) { '1' } else { '0' } }
      elseif ($v -is [guid]) { $s = $v.ToString() }
      else { $s = [string]::Format($inv, '{0}', $v) }
      $o[$n] = $s
    }
    [void]$rows.Add($o)
  }
  $r.Close(); return ,$rows
}

# find the database and company
$master = Open-Db 'master'
$dbs = Query $master "select name from sys.databases where database_id > 4 and state = 0"
$master.Close()
$found = $null
foreach ($d in $dbs) {
  if ($Database -and $d.name -ne $Database) { continue }
  try {
    $c = Open-Db $d.name
    $t = Query $c ("select name from sys.tables where name like '%`$G_L Entry`$" + $BaseAppId + "'")
    if ($t.Count -gt 0) { $found = @{ db = $d.name; conn = $c; tables = $t }; break }
    $c.Close()
  } catch { }
}
if (-not $found) { throw 'no database with a Base App G/L Entry table' }
$prefixes = @($found.tables | ForEach-Object { $_.name.Substring(0, $_.name.IndexOf('$G_L Entry$')) })
$cronus = @($prefixes | Where-Object { $_ -match 'CRONUS' })
$prefix = if ($Company) { SqlName $Company } elseif ($cronus.Count -gt 0) { $cronus[0] } else { $prefixes[0] }
$conn = $found.conn
# table name -> SQL name: the Base App's, else the one other app that defines it (tables moved out of the Base App,
# e.g. Source Code Setup in 29.x); $ext companion tables never match
$sqlTables = @{}; foreach ($r in (Query $conn "select name from sys.tables")) { $sqlTables[$r.name] = 1 }
function T([string]$name) {
  $stem = $prefix + '$' + (SqlName $name) + '$'
  if ($sqlTables.ContainsKey($stem + $BaseAppId)) { return "[" + $stem + $BaseAppId + "]" }
  $m = @($sqlTables.Keys | Where-Object { $_.StartsWith($stem) -and $_.Length -eq $stem.Length + 36 })
  if ($m.Count -gt 0) { return "[" + $m[0] + "]" }
  throw "no SQL table for '$name' (company $prefix)"
}

$out = [ordered]@{ side = 'bc'; server = $SqlServer; database = $found.db; companyPrefix = $prefix; companies = $prefixes; at = (Get-Date).ToUniversalTime().ToString('s') + 'Z' }
if ($Mode -eq 'probe') {
  $out.version = (Query $conn "select @@version v")[0].v
  $out.workdateHints = Query $conn ("select max([Posting Date]) maxPosting, min([Posting Date]) minPosting, count(*) n from " + (T 'G/L Entry'))
  $out.accountingPeriods = Query $conn ("select min([Starting Date]) firstStart, max([Starting Date]) lastStart from " + (T 'Accounting Period'))
}
elseif ($Mode -eq 'baseline') {
  $e = [ordered]@{}
  foreach ($k in $Tables.Keys) { $e[$k] = [int64](Query $conn ("select isnull(max([Entry No_]),0) m from " + (T $Tables[$k])))[0].m }
  $out.entryNo = $e
  $out.transactionNo = [int64](Query $conn ("select isnull(max([Transaction No_]),0) m from " + (T 'G/L Entry')))[0].m
}
elseif ($Mode -eq 'read') {
  $base = $BaseJson | ConvertFrom-Json
  $tabs = [ordered]@{}; $ids = @{}
  foreach ($k in $Tables.Keys) {
    $rows = Query $conn ("select * from " + (T $Tables[$k]) + " where [Entry No_] > " + $base.entryNo.$k + " order by [Entry No_]")
    foreach ($r in $rows) { $r.Remove('timestamp'); if ($r['Dimension Set ID'] -and $r['Dimension Set ID'] -ne '0') { $ids[$r['Dimension Set ID']] = 1 } }
    $tabs[$k] = [ordered]@{ name = $Tables[$k]; rows = $rows }
  }
  $out.tables = $tabs
  $ds = [ordered]@{}
  if ($ids.Count -gt 0) {
    $rows = Query $conn ("select [Dimension Set ID] id, [Dimension Code] d, [Dimension Value Code] v from " + (T 'Dimension Set Entry') + " where [Dimension Set ID] in (" + (($ids.Keys | Sort-Object) -join ',') + ") order by 1, 2")
    foreach ($r in $rows) { if ($ds.Contains($r.id)) { $ds[$r.id] += ';' + $r.d + '=' + $r.v } else { $ds[$r.id] = $r.d + '=' + $r.v } }
  }
  $out.dimsets = $ds
  # columns of the extension tables, for the report (they are not compared)
  $ext = Query $conn ("select t.name tname, c.name cname from sys.tables t join sys.columns c on c.object_id = t.object_id where t.name like '" + $prefix + "`$%`$ext' order by 1")
  $out.extColumns = $ext.Count
}
elseif ($Mode -eq 'setup') {
  $spec = $SetupJson | ConvertFrom-Json
  $tabs = [ordered]@{}
  foreach ($t in $spec) {
    $sql = "select * from " + (T $t.name)
    if ($t.wc) { $sql += " where [" + $t.wc + "] in (" + (($t.wv | ForEach-Object { "'" + $_ + "'" }) -join ',') + ")" }
    $rows = Query $conn $sql
    foreach ($r in $rows) { $r.Remove('timestamp') }
    $tabs[$t.id] = [ordered]@{ name = $t.name; rows = $rows }
  }
  $out.tables = $tabs
}
$conn.Close()
$json = $out | ConvertTo-Json -Depth 6 -Compress
[regex]::Replace($json, '[^\x00-\x7F]', { param($m) '\u{0:x4}' -f [int][char]$m.Value })
