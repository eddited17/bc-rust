# tools/probes: wait until the background sub-runs '<run id>-1' .. '<run id>-<n>' have finished (table
# "BCR Probe Request", Status Done or Failed). Runs INSIDE the BC container with Windows PowerShell (SQL, Windows
# authentication). Prints WAIT progress lines and one STATUS line per sub-run.
param(
    [Parameter(Mandatory = $true)][string]$RunId,
    [Parameter(Mandatory = $true)][int]$Groups,
    [int]$TimeoutSec = 7200,
    [string]$Server = 'localhost\SQLEXPRESS',
    [string]$Database = 'CRONUS'
)
$ErrorActionPreference = 'Stop'
$c = New-Object System.Data.SqlClient.SqlConnection "Server=$Server;Database=$Database;Integrated Security=true"
$c.Open()
$q = $c.CreateCommand()
$q.CommandText = "select name from sys.tables where name like 'BCR Probe Request$%'"
$req = $q.ExecuteScalar()
$q.CommandText = "select name from sys.tables where name like 'BCR Probe Line$%'"
$lines = $q.ExecuteScalar()
$q.CommandText = "select [Run Id], [Status], [Message], (select isnull(sum([Lines]), 0) from [$lines] l where l.[Run Id] = r.[Run Id]) " +
    "from [$req] r where [Run Id] like @p order by [Run Id]"
$q.Parameters.AddWithValue('@p', $RunId.ToUpperInvariant() + '-%') | Out-Null
$sw = [Diagnostics.Stopwatch]::StartNew()
$last = ''
while ($true) {
    $r = $q.ExecuteReader()
    $rows = @()
    while ($r.Read()) { $rows += , @($r.GetString(0), $r.GetString(1), $r.GetString(2), $r.GetValue(3)) }
    $r.Close()
    $finished = @($rows | Where-Object { $_[1] -eq 'Done' -or $_[1] -eq 'Failed' }).Count
    $state = ($rows | ForEach-Object { "$($_[0]) $($_[1]) $($_[3]) lines" }) -join '; '
    if ($state -ne $last) { "WAIT {0:N0}s {1}" -f $sw.Elapsed.TotalSeconds, $state; $last = $state }
    if ($rows.Count -ge $Groups -and $finished -ge $Groups) { break }
    if ($sw.Elapsed.TotalSeconds -gt $TimeoutSec) { "WAIT-TIMEOUT after $TimeoutSec s"; break }
    Start-Sleep -Seconds 3
}
foreach ($row in $rows) { "STATUS $($row[0]) $($row[1]) $($row[2])" }
$c.Close()
