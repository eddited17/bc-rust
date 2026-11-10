# tools/probes: run probe codeunits through web service BCRProbes (codeunit 50401 "BCR Probe Runner", OData V4
# unbound action RunProbes) and copy the run's JSON lines to a file. Runs INSIDE the BC container
# (docker exec -i bcside pwsh -File ...), so the request never crosses the VM's port forward; the response is one
# short line, the lines themselves are read with SQL (ct-fetch.ps1). The BC user name and password come on stdin.
# Contexts are ';'-separated; a context's 'locale=xx-YY' pair is sent as the Accept-Language header, which sets the
# new session's language and format region, so contexts are grouped by locale into one request (sub-run) each.
# -Background: call RunProbesBackground instead (the run happens in a background session; needed for report
# rendering, which must not run inside a web service request), then wait for the sub-runs with ct-wait.ps1.
# A background session takes its language and region from the user's settings, not from Accept-Language.
# Output markers read by probes.py: INVOKED (one per sub-run), WAIT/STATUS (background), GROUPS <n>, OUT <path>.
param(
    [Parameter(Mandatory = $true)][string]$RunId,
    [Parameter(Mandatory = $true)][string]$Probes,
    [string]$Contexts = '',
    [string]$Company = 'CRONUS International Ltd.',
    [Parameter(Mandatory = $true)][string]$Out,
    [string]$BaseUrl = 'http://localhost:7048/BC',
    [int]$TimeoutSec = 900,
    [switch]$Background,
    [int]$WaitSec = 7200
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$user = [Console]::In.ReadLine()
$pw = [Console]::In.ReadLine()
$auth = 'Basic ' + [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("${user}:${pw}"))
$action = if ($Background) { 'RunProbesBackground' } else { 'RunProbes' }
$uri = "$BaseUrl/ODataV4/BCRProbes_$action" + '?company=' + [Uri]::EscapeDataString($Company)
$groups = [ordered]@{}
foreach ($c in ($Contexts -split ';')) {
    $loc = ''
    foreach ($kv in ($c -split '&')) { if ($kv -like 'locale=*') { $loc = $kv.Substring(7) } }
    if (-not $groups.Contains($loc)) { $groups[$loc] = New-Object System.Collections.Generic.List[string] }
    $groups[$loc].Add($c)
}
$k = 0
foreach ($loc in $groups.Keys) {
    $k++
    $headers = @{ Authorization = $auth }
    if ($loc) { $headers['Accept-Language'] = $loc }
    $body = @{ args = "$RunId-$k|$Probes|" + ($groups[$loc] -join ';') } | ConvertTo-Json -Compress
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $r = $null
    try {
        $r = Invoke-RestMethod -Method Post -Uri $uri -Headers $headers -Body $body `
            -ContentType 'application/json; charset=utf-8' -TimeoutSec $TimeoutSec
        "INVOKED {0:N1}s {1}" -f $sw.Elapsed.TotalSeconds, $r.value
    } catch {
        "INVOKE-FAILED after {0:N1}s: {1} {2}" -f $sw.Elapsed.TotalSeconds, $_.Exception.Message, $_.ErrorDetails.Message
    }
}
"GROUPS $k"
if ($Background) {
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'ct-wait.ps1') -RunId $RunId -Groups $k -TimeoutSec $WaitSec
}
& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'ct-fetch.ps1') -RunId $RunId -Out $Out
