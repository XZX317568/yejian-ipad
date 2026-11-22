<#
.SYNOPSIS
  把多个变体的同一时刻抽帧拼成一张对照图（PNG），降低裁决成本。

.DESCRIPTION
  裁决时看 8 秒视频要拖进度条；一张带时间戳的拼图能一眼看完。
  行 = 变体（A/B/C），列 = 关键时刻。

.EXAMPLE
  ./tools/contact-sheet.ps1 -Videos a.mp4,b.mp4,c.mp4 -Out contact.png
#>
param(
  [string[]]$Videos = @(),
  [double[]]$Times = @(2.6, 3.2, 4.0, 5.2),
  [string]$Out = '',
  [int]$Width = 512
)

$ErrorActionPreference = 'Stop'
$ffmpeg = 'D:\ffmpeg-2026-05-13-git-a327bc0561-full_build\bin\ffmpeg.exe'
if (-not (Test-Path $ffmpeg)) { throw "ffmpeg not found: $ffmpeg" }
if ($Videos.Count -eq 0) { throw 'need at least one video' }

$root = Split-Path -Parent $PSScriptRoot
$tmp = Join-Path $root ('.tools\contact-' + (Get-Date -Format 'HHmmss'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
if (-not $Out) { $Out = Join-Path (Split-Path -Parent (Resolve-Path $Videos[0]).Path) 'contact.png' }

$rows = @()
foreach ($v in $Videos) {
  $stills = @()
  for ($i = 0; $i -lt $Times.Count; $i++) {
    $png = Join-Path $tmp ("r" + $rows.Count + "-c" + $i + ".png")
    & $ffmpeg -y -loglevel error -ss $Times[$i] -i $v -frames:v 1 -vf ("scale=" + $Width + ":-1") $png
    if ($LASTEXITCODE -ne 0) { throw ("still failed: " + $v + " @ " + $Times[$i]) }
    $stills += $png
  }
  $rows += ,$stills
}

# 每行 hstack，再把各行 vstack
$inputs = @()
foreach ($row in $rows) { foreach ($s in $row) { $inputs += @('-i', $s) } }
$n = $Times.Count
$chain = @()
$rowLabels = @()
for ($r = 0; $r -lt $rows.Count; $r++) {
  $parts = @()
  for ($c = 0; $c -lt $n; $c++) { $parts += ("[" + ($r * $n + $c) + ":v]") }
  $label = "row" + $r
  $chain += ($parts -join '') + ("hstack=inputs=" + $n + "[" + $label + "]")
  $rowLabels += $label
}
$stack = ''
foreach ($l in $rowLabels) { $stack += ("[" + $l + "]") }
$chain += $stack + ("vstack=inputs=" + $rows.Count + "[out]")

& $ffmpeg -y -loglevel error @inputs -filter_complex ($chain -join ';') -map '[out]' $Out
if ($LASTEXITCODE -ne 0) { throw 'contact sheet failed' }
Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
Write-Host ("contact sheet -> " + $Out)
