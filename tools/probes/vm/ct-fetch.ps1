# tools/probes: copy the JSON lines of one probe run from table "BCR Probe Line" to a file. Runs INSIDE the BC
# container with Windows PowerShell (System.Data.SqlClient, Windows authentication on the container's SQL Server).
param(
    [Parameter(Mandatory = $true)][string]$RunId,
    [Parameter(Mandatory = $true)][string]$Out,
    [string]$Server = 'localhost\SQLEXPRESS',
    [string]$Database = 'CRONUS'
)
$ErrorActionPreference = 'Stop'
$c = New-Object System.Data.SqlClient.SqlConnection "Server=$Server;Database=$Database;Integrated Security=true"
$c.Open()
$q = $c.CreateCommand()
$q.CommandText = "select name from sys.tables where name like 'BCR Probe Line$%'"
$t = $q.ExecuteScalar()
if (-not $t) { "FETCH-FAILED no table BCR Probe Line"; exit 3 }
# the run's sub-runs are '<run id>-1', '<run id>-2', ... (one per locale)
$q.CommandText = "select [Data] from [$t] where [Run Id] = @r or [Run Id] like @p order by len([Run Id]), [Run Id], [Chunk]"
$q.Parameters.AddWithValue('@r', $RunId.ToUpperInvariant()) | Out-Null
$q.Parameters.AddWithValue('@p', $RunId.ToUpperInvariant() + '-%') | Out-Null
$r = $q.ExecuteReader()
$fs = [IO.File]::Create($Out)
$chunks = 0
while ($r.Read()) {
    if (-not $r.IsDBNull(0)) { $b = [byte[]]$r.GetValue(0); $fs.Write($b, 0, $b.Length); $chunks++ }
}
$fs.Close(); $r.Close()
"OUT $Out $((Get-Item $Out).Length) bytes in $chunks chunks"
# binary outputs ("BCR Probe File") go to <Out without .jsonl>.files\<probe id>\<name>
$q.CommandText = "select name from sys.tables where name like 'BCR Probe File$%'"
$ft = $q.ExecuteScalar()
$files = 0
if ($ft) {
    try {
        $q.CommandText = "select [Probe Id], [Name], [Data] from [$ft] where [Run Id] = @r or [Run Id] like @p order by len([Run Id]), [Run Id], [Seq]"
        $r = $q.ExecuteReader()
        $dir = $Out -replace '\.jsonl$', '.files'
        while ($r.Read()) {
            $pd = Join-Path $dir ($r.GetString(0) -replace '[\\/:*?"<>|]', '_')
            if (-not (Test-Path $pd)) { New-Item -ItemType Directory -Force -Path $pd | Out-Null }
            # an empty byte[] coming out of an if expression would unroll to $null, so assign in each branch
            if ($r.IsDBNull(2)) { $b = New-Object byte[] 0 } else { $b = [byte[]]$r.GetValue(2) }
            [IO.File]::WriteAllBytes((Join-Path $pd ($r.GetString(1) -replace '[\\/:*?"<>|]', '_')), $b)
            $files++
        }
        $r.Close()
        if ($files) { "FILES $files $dir" }
    } catch { "FILES-FAILED after $files files: $($_.Exception.Message)" }
}
$c.Close()
