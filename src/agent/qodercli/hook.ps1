# Luvus Qoder CLI integration. Qoder passes SessionStart JSON through stdin;
# the Luvus binary parses only its bounded session_id field.

param([string]$Action = "")

if ($Action -ne "session") { exit 0 }
if (
    $env:LUVUS_ENV -ne "1" -or
    [string]::IsNullOrWhiteSpace($env:LUVUS_SOCKET_PATH) -or
    [string]::IsNullOrWhiteSpace($env:LUVUS_PANE_ID)
) {
    Write-Output "{}"
    exit 0
}

$luvus = if ([string]::IsNullOrWhiteSpace($env:LUVUS_BIN_PATH)) { "luvus" } else { $env:LUVUS_BIN_PATH }
try {
    & $luvus integration hook qodercli 2>$null
    if ($LASTEXITCODE -ne 0) { Write-Output "{}" }
} catch {
    Write-Output "{}"
}
