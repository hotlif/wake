param(
    [Parameter(Mandatory=$true, Position=0)]
    [string]$Exe,
    [Parameter(Position=1, ValueFromRemainingArguments=$true)]
    [string[]]$ArgumentList
)

function ConvertTo-ProcessArgument([string]$Argument) {
    if ($Argument.Length -gt 0 -and $Argument -notmatch '[\s"]') {
        return $Argument
    }

    # Start-Process accepts one Windows command-line string. Preserve argument
    # boundaries, embedded quotes, and trailing backslashes per CommandLineToArgvW.
    $escaped = [regex]::Replace($Argument, '(\\*)"', '$1$1\"')
    $escaped = [regex]::Replace($escaped, '(\\+)$', '$1$1')
    return '"' + $escaped + '"'
}

$argStr = ($ArgumentList | ForEach-Object { ConvertTo-ProcessArgument $_ }) -join ' '

try {
    $p = Start-Process -FilePath $Exe -ArgumentList $argStr -NoNewWindow -PassThru -ErrorAction Stop
} catch {
    Write-Error "failed to start benchmark process '$Exe': $($_.Exception.Message)"
    exit 1
}

$peak = 0
while (!$p.HasExited) {
    $p.Refresh()
    $ws = $p.WorkingSet64
    if ($ws -gt $peak) { $peak = $ws }
    Start-Sleep -Milliseconds 20
}
$p.WaitForExit()

Write-Output "__PEAK_MB__:$([math]::Round($peak / 1MB))"
exit $p.ExitCode
