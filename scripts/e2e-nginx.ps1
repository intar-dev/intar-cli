$ErrorActionPreference = "Stop"

$intar = $env:INTAR_BIN
if (-not $intar) {
  $intar = "target\\debug\\intar.exe"
}

Write-Host "Listing scenarios..."
& $intar list --dir scenarios

Write-Host "Printing help..."
& $intar --help
