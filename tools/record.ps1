<#
.SYNOPSIS
  录制端到端动画，并把参数变体并排成一段对比视频。

.DESCRIPTION
  依赖 acceptance 的默认全量验收路径：它会把书架抽出 → 摊开 → 详情 → 阅读的
  连续帧写到 <输出>/motion/frame-NNN.png。本脚本把这些帧交给 ffmpeg 编码成 mp4；
  传入多个变体时再横向拼接成 compare.mp4，供用户只看编号做裁决。

.EXAMPLE
  # 单个变体
  ./tools/record.ps1 -Out ..\artifacts\rust\record\base

.EXAMPLE
  # 三个参数变体并排
  ./tools/record.ps1 -Variants params.a.json,params.b.json,params.c.json -Out ..\artifacts\rust\record\r1
#>
param(
  [string[]]$Variants = @(),
  [string]$Out = '..\artifacts\rust\record',
  [int]$Fps = 30
)

$ErrorActionPreference = 'Stop'
$ffmpeg = 'D:\ffmpeg-2026-05-13-git-a327bc0561-full_build\bin\ffmpeg.exe'
if (-not (Test-Path $ffmpeg)) { throw "ffmpeg not found: $ffmpeg" }

$repo = Split-Path -Parent $PSScriptRoot
$rust = Join-Path $repo 'rust-reader'
$outDir = if ([System.IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $rust $Out }
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

function Invoke-Acceptance([string]$dir) {
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  Push-Location $rust
  try {
    $env:TMP = Join-Path $repo '.tools\loop-tmp'
    $env:TEMP = $env:TMP
    New-Item -ItemType Directory -Force -Path $env:TMP | Out-Null
    cargo run --locked -j 1 --features acceptance --bin acceptance -- $dir 2>&1 | Select-Object -Last 3
    if ($LASTEXITCODE -ne 0) { throw "acceptance failed in $dir (exit $LASTEXITCODE)" }
  } finally { Pop-Location }
}

$labels = @()
$videos = @()
if ($Variants.Count -eq 0) {
  $dir = $outDir
  Invoke-Acceptance $dir
  $labels = @('base'); $videos = @($dir)
} else {
  $i = 0
  foreach ($v in $Variants) {
    $i++
    $label = [char](64 + $i)          # A, B, C
    $dir = Join-Path $outDir $label
    $saved = $env:YEJIAN_PARAMS
    $env:YEJIAN_PARAMS = (Resolve-Path $v).Path
    try { Invoke-Acceptance $dir } finally { $env:YEJIAN_PARAMS = $saved }
    $labels += $label; $videos += $dir
  }
}

# 每个变体编码一段 mp4
$enc = @()
for ($i = 0; $i -lt $videos.Count; $i++) {
  $frames = Join-Path $videos[$i] 'motion'
  if (-not (Test-Path $frames)) { Write-Warning "no motion frames in $($videos[$i])"; continue }
  $mp4 = Join-Path $videos[$i] "variant-$($labels[$i]).mp4"
  & $ffmpeg -y -loglevel error -framerate $Fps -i (Join-Path $frames 'frame-%03d.png') -c:v libx264 -pix_fmt yuv420p $mp4
  if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed for $($labels[$i])" }
  $enc += $mp4
  Write-Host "encoded $mp4"
}

# 多变异步并排
if ($enc.Count -gt 1) {
  $cmp = Join-Path $outDir 'compare.mp4'
  $inputs = @(); for ($i = 0; $i -lt $enc.Count; $i++) { $inputs += @('-i', $enc[$i]) }
  $stack = ($enc | ForEach-Object { "[$($enc.IndexOf($_)):v]" }) -join ''
  & $ffmpeg -y -loglevel error @inputs -filter_complex "$($stack)hstack=inputs=$($enc.Count)" -c:v libx264 -pix_fmt yuv420p $cmp
  if ($LASTEXITCODE -ne 0) { throw 'ffmpeg hstack failed' }
  Write-Host "compare -> $cmp"
}

Write-Host ("variants: " + ($labels -join ', '))
Write-Host ("output:   " + $outDir)
