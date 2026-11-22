<#
.SYNOPSIS
  量"书"（高饱和像素）的包围盒，用数字判断扇形构图是否居中。
#>
param(
  [Parameter(Mandatory=$true)][string]$Png,
  [int]$X = 0, [int]$Y = 0, [int]$W = 0, [int]$H = 0,
  [int]$Saturation = 24
)

Add-Type -AssemblyName System.Drawing
$img = [System.Drawing.Bitmap]::FromFile((Resolve-Path $Png))
try {
  if ($W -le 0) { $W = $img.Width - $X }
  if ($H -le 0) { $H = $img.Height - $Y }
  $minX = [int]::MaxValue; $minY = [int]::MaxValue; $maxX = -1; $maxY = -1
  for ($yy = $Y; $yy -lt ($Y + $H); $yy += 2) {
    for ($xx = $X; $xx -lt ($X + $W); $xx += 2) {
      $c = $img.GetPixel($xx, $yy)
      $mx = [math]::Max($c.R, [math]::Max($c.G, $c.B))
      $mn = [math]::Min($c.R, [math]::Min($c.G, $c.B))
      if (($mx - $mn) -gt $Saturation) {
        if ($xx -lt $minX) { $minX = $xx }
        if ($xx -gt $maxX) { $maxX = $xx }
        if ($yy -lt $minY) { $minY = $yy }
        if ($yy -gt $maxY) { $maxY = $yy }
      }
    }
  }
  if ($maxX -lt 0) { Write-Host "no saturated content"; exit 1 }
  $left = $minX - $X
  $right = ($X + $W) - $maxX
  $top = $minY - $Y
  $bottom = ($Y + $H) - $maxY
  $offX = [math]::Round((($minX + $maxX) / 2) - (($X + $X + $W) / 2))
  Write-Host ("png=" + (Split-Path -Leaf $Png) + " crop=(" + $X + "," + $Y + ") " + $W + "x" + $H)
  Write-Host ("books bbox=(" + $minX + "," + $minY + ")-(" + $maxX + "," + $maxY + ")  " + ($maxX - $minX) + "x" + ($maxY - $minY))
  Write-Host ("margins  L=" + $left + "  R=" + $right + "  T=" + $top + "  B=" + $bottom + "   centerOffsetX=" + $offX)
} finally { $img.Dispose() }
