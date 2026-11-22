<#
    Generate Yejian/Assets.xcassets/AppIcon.appiconset from the same book artwork
    that ships with the Android vector icon
    (platform/android/app/src/main/res/mipmap-anydpi/ic_launcher.xml).

    The geometry below is a 1:1 copy of that vector: same 108x108 viewport,
    same fill colours, same polygon points. Only the placement changes: iOS
    icons must be square, opaque and un-rounded, so the dark background fills
    the whole canvas and the book art is centred at ~58% of the icon width.

    Requires Windows PowerShell 5.1+ (System.Drawing / GDI+). No external tools.

    Usage:
        pwsh -File tools/generate-appicon.ps1            # writes the asset catalog
        pwsh -File tools/generate-appicon.ps1 -Preview   # also writes .tools/appicon-preview.png
#>
[CmdletBinding()]
param(
    [string]$OutDir = (Join-Path $PSScriptRoot '..\Yejian\Assets.xcassets\AppIcon.appiconset'),
    [switch]$Preview
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

# --- artwork -----------------------------------------------------------------
# 108dp viewport, identical to the Android vector.
$Back = '#14161C'   # ic_launcher_background
$Left = '#F4EBD8'
$Right = '#EADFC5'
$Spine = '#C7B998'
$Line = '66,8E,84,74'   # AARRGGBB #668E8474 (40% ink)
$Mark = '#D9A93C'

$Shapes = @(
    @{ Fill = $Back;  Points = @((0,0),(108,0),(108,108),(0,108)) },
    @{ Fill = $Left;  Points = @((27,34),(51,30),(51,78),(27,82)) },
    @{ Fill = $Right; Points = @((57,30),(81,34),(81,82),(57,78)) },
    @{ Fill = $Spine; Points = @((51,30),(57,30),(57,78),(51,78)) },
    @{ Fill = $Line;  Points = @((33,42),(45,40.6),(45,43.2),(33,44.6)) },
    @{ Fill = $Line;  Points = @((33,48),(45,46.6),(45,49.2),(33,50.6)) },
    @{ Fill = $Line;  Points = @((33,54),(41,53),(41,55.6),(33,56.6)) },
    @{ Fill = $Line;  Points = @((63,40.6),(75,42),(75,44.6),(63,43.2)) },
    @{ Fill = $Line;  Points = @((63,46.6),(75,48),(75,50.6),(63,49.2)) },
    @{ Fill = $Line;  Points = @((67,52.6),(75,53.6),(75,56.2),(67,55.2)) },
    @{ Fill = $Mark;  Points = @((60,30),(67,30.9),(67,47),(63.5,43),(60,47)) }
)

function ConvertTo-Color([string]$spec) {
    if ($spec.Contains(',')) {
        # "AA,RR,GG,BB" in hex, same digits as the Android #AARRGGBB literal.
        $p = $spec -split ','
        return [System.Drawing.Color]::FromArgb(
            [Convert]::ToInt32($p[0], 16), [Convert]::ToInt32($p[1], 16),
            [Convert]::ToInt32($p[2], 16), [Convert]::ToInt32($p[3], 16))
    }
    return [System.Drawing.ColorTranslator]::FromHtml($spec)
}

$Palette = @{}
foreach ($s in $Shapes) { if (-not $Palette.ContainsKey($s.Fill)) { $Palette[$s.Fill] = ConvertTo-Color $s.Fill } }

# Book art width on the 1024pt canvas: 58% of the icon. 54 units wide (27..81).
$ArtWidthPx = 1024.0 * 0.58
$UnitToPx = $ArtWidthPx / 54.0      # px per viewport unit at 1024pt
$CenterUnit = 54.0, 56.0            # centre of the book glyph

function New-IconBitmap([int]$size) {
    # Draw at 4x and downsample: GDI+ polygon edges are aliased without this.
    $ss = 4
    $big = $size * $ss
    $scale = $UnitToPx * $size / 1024.0 * $ss
    $cx = $big / 2.0
    $cy = $big / 2.0

    $hi = New-Object System.Drawing.Bitmap($big, $big, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($hi)
    try {
        $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $g.Clear([System.Drawing.ColorTranslator]::FromHtml($Back))

        foreach ($s in $Shapes) {
            if ($s.Fill -eq $Back) { continue }   # background already painted full-bleed
            $pts = [System.Drawing.PointF[]]@(
                foreach ($pt in $s.Points) {
                    [System.Drawing.PointF]::new(
                        [single]($cx + ($pt[0] - $CenterUnit[0]) * $scale),
                        [single]($cy + ($pt[1] - $CenterUnit[1]) * $scale))
                }
            )
            $brush = New-Object System.Drawing.SolidBrush($Palette[$s.Fill])
            try { $g.FillPolygon($brush, $pts) } finally { $brush.Dispose() }
        }
    } finally { $g.Dispose() }

    # App Store rejects icons whose PNG carries an alpha channel, so the final
    # bitmap is plain 24bpp RGB even though the artwork is fully opaque anyway.
    $dst = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
    $gd = [System.Drawing.Graphics]::FromImage($dst)
    try {
        $gd.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
        $gd.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
        $gd.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $gd.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $gd.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $attr = New-Object System.Drawing.Imaging.ImageAttributes
        try {
            $attr.SetWrapMode([System.Drawing.Drawing2D.WrapMode]::TileFlipXY)
            $rect = New-Object System.Drawing.Rectangle(0, 0, $size, $size)
            $gd.DrawImage($hi, $rect, 0, 0, $big, $big, [System.Drawing.GraphicsUnit]::Pixel, $attr)
        } finally { $attr.Dispose() }
    } finally { $gd.Dispose(); $hi.Dispose() }
    return $dst
}

# --- asset catalog -----------------------------------------------------------
# Classic (non single-size) Xcode 15 catalog: every iPhone/iPad slot is a real
# PNG so older Xcode versions and App Store validation keep working.
$Slots = @(
    @{ Idiom='iphone'; Size='20x20';    Scale='2x'; Px=40  },
    @{ Idiom='iphone'; Size='20x20';    Scale='3x'; Px=60  },
    @{ Idiom='iphone'; Size='29x29';    Scale='2x'; Px=58  },
    @{ Idiom='iphone'; Size='29x29';    Scale='3x'; Px=87  },
    @{ Idiom='iphone'; Size='40x40';    Scale='2x'; Px=80  },
    @{ Idiom='iphone'; Size='40x40';    Scale='3x'; Px=120 },
    @{ Idiom='iphone'; Size='60x60';    Scale='2x'; Px=120 },
    @{ Idiom='iphone'; Size='60x60';    Scale='3x'; Px=180 },
    @{ Idiom='ipad';   Size='20x20';    Scale='1x'; Px=20  },
    @{ Idiom='ipad';   Size='20x20';    Scale='2x'; Px=40  },
    @{ Idiom='ipad';   Size='29x29';    Scale='1x'; Px=29  },
    @{ Idiom='ipad';   Size='29x29';    Scale='2x'; Px=58  },
    @{ Idiom='ipad';   Size='40x40';    Scale='1x'; Px=40  },
    @{ Idiom='ipad';   Size='40x40';    Scale='2x'; Px=80  },
    @{ Idiom='ipad';   Size='76x76';    Scale='1x'; Px=76  },
    @{ Idiom='ipad';   Size='76x76';    Scale='2x'; Px=152 },
    @{ Idiom='ipad';   Size='83.5x83.5';Scale='2x'; Px=167 },
    @{ Idiom='ios-marketing'; Size='1024x1024'; Scale='1x'; Px=1024 }
)

$OutDir = [System.IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$images = @()
foreach ($slot in $Slots) {
    $name = 'icon-{0}-{1}@{2}.png' -f $slot.Idiom, ($slot.Size -replace '\.', '_'), $slot.Scale
    $bmp = New-IconBitmap ([int]$slot.Px)
    try { $bmp.Save((Join-Path $OutDir $name), [System.Drawing.Imaging.ImageFormat]::Png) }
    finally { $bmp.Dispose() }
    $images += [ordered]@{ size = $slot.Size; idiom = $slot.Idiom; filename = $name; scale = $slot.Scale }
    if ($Preview -and $slot.Idiom -eq 'ios-marketing') {
        $bmp2 = New-IconBitmap 1024
        try {
            $pdir = Join-Path $PSScriptRoot '..\..\..\..\.tools'
            New-Item -ItemType Directory -Force -Path $pdir | Out-Null
            $bmp2.Save((Join-Path $pdir 'ios-appicon-preview.png'), [System.Drawing.Imaging.ImageFormat]::Png)
        } finally { $bmp2.Dispose() }
    }
}

$catalog = [ordered]@{
    images = $images
    info   = [ordered]@{ author = 'xcode'; version = 1 }
}
$json = $catalog | ConvertTo-Json -Depth 6
[System.IO.File]::WriteAllText((Join-Path $OutDir 'Contents.json'), $json + "`n", (New-Object System.Text.UTF8Encoding($false)))

$rootCatalog = [ordered]@{
    info = [ordered]@{ author = 'xcode'; version = 1 }
}
$rootJson = $rootCatalog | ConvertTo-Json -Depth 3
$rootPath = Join-Path (Split-Path $OutDir -Parent) 'Contents.json'
[System.IO.File]::WriteAllText($rootPath, $rootJson + "`n", (New-Object System.Text.UTF8Encoding($false)))

Write-Host ("Wrote {0} PNGs to {1}" -f $Slots.Count, $OutDir)
