# tools/probes: compile the generated "bc-rust probes" app with the container's AL compiler and publish it with the
# service tier's admin cmdlets. Runs INSIDE the BC container (docker exec bcside pwsh -File ...); the project folder
# lives under C:\ProgramData\BcContainerHelper, which the container shares with its host.
# Output markers read by probes.py: ALC <compiler line>, COMPILED, PUBLISHED <version>, UNPUBLISHED <version>.
param(
    [Parameter(Mandatory = $true)][string]$Proj,
    [Parameter(Mandatory = $true)][string]$Symbols,
    [string]$ServerInstance = 'BC'
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$app = Get-Content -Raw (Join-Path $Proj 'app.json') | ConvertFrom-Json
$outDir = Join-Path $Proj 'output'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$file = Join-Path $outDir ("{0}_{1}_{2}.app" -f $app.publisher, $app.name, $app.version)
$alc = Get-ChildItem 'C:\build\vsix\extension\bin' -Recurse -Filter alc.exe | Select-Object -First 1 -ExpandProperty FullName
$sw = [Diagnostics.Stopwatch]::StartNew()
& $alc "/project:$Proj" "/packagecachepath:$Symbols" "/out:$file" 2>&1 | ForEach-Object { "ALC $_" }
if ($LASTEXITCODE -ne 0 -or -not (Test-Path $file)) { "COMPILE-FAILED (alc exit $LASTEXITCODE)"; exit 2 }
"COMPILED in {0:N1} s" -f $sw.Elapsed.TotalSeconds
$sw.Restart()
$admin = Get-ChildItem 'C:\Program Files\Microsoft Dynamics NAV\*\Service\Admin' | Select-Object -First 1 -ExpandProperty FullName
Import-Module (Join-Path $admin 'Microsoft.BusinessCentral.Management.psd1') -WarningAction SilentlyContinue
Import-Module (Join-Path $admin 'Microsoft.BusinessCentral.Apps.Management.dll') -WarningAction SilentlyContinue
$before = @(Get-NAVAppInfo -ServerInstance $ServerInstance -Name $app.name -Publisher $app.publisher -TenantSpecificProperties -Tenant default)
Publish-NAVApp -ServerInstance $ServerInstance -Path $file -SkipVerification
Sync-NAVApp -ServerInstance $ServerInstance -Name $app.name -Publisher $app.publisher -Version $app.version -Mode ForceSync -Force
if ($before | Where-Object { $_.IsInstalled }) {
    Start-NAVAppDataUpgrade -ServerInstance $ServerInstance -Name $app.name -Publisher $app.publisher -Version $app.version -Force
} else {
    Install-NAVApp -ServerInstance $ServerInstance -Name $app.name -Publisher $app.publisher -Version $app.version -Force
}
"PUBLISHED $($app.version) in {0:N1} s" -f $sw.Elapsed.TotalSeconds
foreach ($old in $before | Where-Object { "$($_.Version)" -ne $app.version }) {
    try {
        Unpublish-NAVApp -ServerInstance $ServerInstance -Name $old.Name -Publisher $old.Publisher -Version $old.Version
        "UNPUBLISHED $($old.Version)"
    } catch { "UNPUBLISH-FAILED $($old.Version): $($_.Exception.Message)" }
}
