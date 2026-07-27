param(
    [Parameter(Mandatory=$true, Position=0)]
    [string]$Exe,
    [Parameter(Position=1, ValueFromRemainingArguments=$true)]
    [string[]]$ArgumentList
)

$argStr = $ArgumentList -join ' '

$p = Start-Process -FilePath $Exe -ArgumentList $argStr -NoNewWindow -PassThru

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
