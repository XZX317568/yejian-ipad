<#
.SYNOPSIS
  证伪验收管线：故意把参数改坏，验收必须至少一处 FAIL。

.DESCRIPTION
  如果坏参数仍然全绿，说明验收根本没覆盖纸张几何 —— 此时不许继续迭代，
  先给对应参数补一条可机判断言，再跑本脚本直到它 FAIL。

.EXAMPLE
  ./tools/kill-test.ps1
#>
param(
  [string]$Out = '..\artifacts\rust\killtest'
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$rust = Join-Path $repo 'rust-reader'
$outDir = if ([System.IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $rust $Out }
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$goodParams = Join-Path $rust 'params.json'
$brokenParams = Join-Path $outDir 'params-broken.json'

# 破坏一：纸张完全平（base=0, arch=0, breeze=0）
$p = Get-Content $goodParams -Raw | ConvertFrom-Json
$p.paper.base = 0
$p.paper.arch = 0
$p.paper.breeze_gain = 0
$p | ConvertTo-Json -Depth 8 | Set-Content $brokenParams -Encoding utf8

Push-Location $rust
try {
  $env:TMP = Join-Path $repo '.tools\loop-tmp'
  $env:TEMP = $env:TMP
  New-Item -ItemType Directory -Force -Path $env:TMP | Out-Null

  Write-Host '=== 1/2 正常参数（应 PASS） ==='
  $env:YEJIAN_PARAMS = $goodParams
  cargo run --locked -j 1 --features acceptance --bin acceptance -- (Join-Path $outDir 'good') 2>&1 | Select-Object -Last 2
  $goodExit = $LASTEXITCODE

  Write-Host '=== 2/2 坏参数（应 FAIL） ==='
  $env:YEJIAN_PARAMS = $brokenParams
  cargo run --locked -j 1 --features acceptance --bin acceptance -- (Join-Path $outDir 'broken') 2>&1 | Select-Object -Last 2
  $brokenExit = $LASTEXITCODE
} finally {
  Remove-Item Env:YEJIAN_PARAMS -ErrorAction SilentlyContinue
  Pop-Location
}

Write-Host ''
Write-Host "good exit   = $goodExit   (期望 0)"
Write-Host "broken exit = $brokenExit   (期望 非 0)"
if ($goodExit -ne 0) { Write-Host 'KILL TEST INVALID: 正常参数都不过，先修验收环境'; exit 2 }
if ($brokenExit -eq 0) {
  Write-Host 'KILL TEST FAILED: 验收看不见"纸张完全是平的"这类错 —— 必须先补纸张几何断言'
  exit 1
}
Write-Host 'KILL TEST PASSED: 验收能抓到坏参数'
