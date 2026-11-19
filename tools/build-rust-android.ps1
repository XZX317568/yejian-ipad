#Requires -Version 7.0
<#
.SYNOPSIS
    Build the Rust cdylib and package the minimal Android shell.

.DESCRIPTION
    Uses cargo-ndk to place libyejian.so into the platform jniLibs directory,
    then runs the platform Gradle wrapper. It never installs the APK and never
    touches the old Flutter app under app/android.

    The Gradle cache and distribution are reused from the project .tools
    directory. Cargo is limited to one job and Gradle to one worker, matching
    the repository build policy.

.EXAMPLE
    .\tools\build-rust-android.ps1
    .\tools\build-rust-android.ps1 -Profile release
    .\tools\build-rust-android.ps1 -ApiLevel 24 -Abi arm64-v8a
    .\tools\build-rust-android.ps1 -SkipRust
#>
[CmdletBinding()]
param(
    [ValidateSet('debug', 'release')]
    [string]$Profile = 'debug',

    [ValidateSet('arm64-v8a', 'armeabi-v7a', 'x86_64', 'x86')]
    [string]$Abi = 'arm64-v8a',

    [string]$Package = 'yejian',
    [string]$LibName = '',

    [string]$AndroidSdk = '',
    [string]$NdkVersion = '28.2.13676358',
    [int]$ApiLevel = 24,

    [switch]$SkipRust,
    [switch]$SkipGradle
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$rustRoot = Join-Path $root 'rust-reader'
$manifest = Join-Path $rustRoot 'Cargo.toml'
$platform = Join-Path $rustRoot 'platform\android'
$jniRoot = Join-Path $platform 'app\src\main\jniLibs'

if ([string]::IsNullOrWhiteSpace($LibName)) {
    $LibName = $Package -replace '-', '_'
}

function Resolve-AndroidSdk {
    param([string]$Explicit)

    $candidates = [System.Collections.Generic.List[string]]::new()
    if ($Explicit) { $candidates.Add($Explicit) }
    if ($env:ANDROID_HOME) { $candidates.Add($env:ANDROID_HOME) }
    if ($env:ANDROID_SDK_ROOT) { $candidates.Add($env:ANDROID_SDK_ROOT) }
    if ($env:LOCALAPPDATA) {
        $candidates.Add((Join-Path $env:LOCALAPPDATA 'Android\sdk'))
    }

    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate)) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    throw '找不到 Android SDK。请设置 ANDROID_HOME，或用 -AndroidSdk 指定路径。'
}

function Resolve-Ndk {
    param([string]$Sdk, [string]$Preferred)

    if ($env:ANDROID_NDK_HOME -and (Test-Path -LiteralPath $env:ANDROID_NDK_HOME)) {
        return (Resolve-Path -LiteralPath $env:ANDROID_NDK_HOME).Path
    }

    $ndkRoot = Join-Path $Sdk 'ndk'
    $preferredPath = Join-Path $ndkRoot $Preferred
    if (Test-Path -LiteralPath $preferredPath) {
        return (Resolve-Path -LiteralPath $preferredPath).Path
    }

    $latest = Get-ChildItem -LiteralPath $ndkRoot -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name |
        Select-Object -Last 1
    if ($latest) {
        Write-Warning "没有找到 NDK $Preferred，改用 $($latest.Name)。"
        return $latest.FullName
    }
    throw "Android SDK 下没有 NDK。请安装 NDK $Preferred，或设置 ANDROID_NDK_HOME。"
}

if (-not (Test-Path -LiteralPath $platform)) {
    throw "找不到 Android 包装工程：$platform"
}
if (-not $SkipRust -and -not (Test-Path -LiteralPath $manifest)) {
    throw "找不到 Rust 工程：$manifest"
}

$sdkPath = Resolve-AndroidSdk -Explicit $AndroidSdk
$ndkPath = Resolve-Ndk -Sdk $sdkPath -Preferred $NdkVersion

$env:ANDROID_HOME = $sdkPath
$env:ANDROID_SDK_ROOT = $sdkPath
$env:ANDROID_NDK_HOME = $ndkPath
$env:ANDROID_NDK_ROOT = $ndkPath
$env:GRADLE_USER_HOME = Join-Path $root '.tools\gradle'
$env:CARGO_BUILD_JOBS = '1'

Write-Host "Android SDK : $sdkPath"
Write-Host "Android NDK : $ndkPath"
Write-Host "Profile     : $Profile ($Abi)"

if (-not $SkipRust) {
    New-Item -ItemType Directory -Force -Path $jniRoot | Out-Null

    # The cdylib links against libc for this API level; a mismatch with the
    # Gradle minSdk produces a library the device refuses to load.
    $targetTriple = switch ($Abi) {
        'arm64-v8a' { 'aarch64-linux-android' }
        'armeabi-v7a' { 'armv7-linux-androideabi' }
        'x86_64' { 'x86_64-linux-android' }
        'x86' { 'i686-linux-android' }
    }
    $installedTargets = & rustup target list --installed 2>$null
    if ($installedTargets -notcontains $targetTriple) {
        throw "没有安装 Rust 目标 $targetTriple。请先运行：rustup target add $targetTriple"
    }

    Push-Location $rustRoot
    try {
        $arguments = @(
            'ndk',
            '-t', $Abi,
            '-p', "$ApiLevel",
            '-o', $jniRoot,
            'build',
            '--lib',
            '-j', '1',
            '-p', $Package
        )
        if ($Profile -eq 'release') {
            $arguments += '--release'
        }
        & cargo @arguments
        if ($LASTEXITCODE -ne 0) {
            throw "cargo ndk 失败，退出码 $LASTEXITCODE。"
        }
    }
    finally {
        Pop-Location
    }
}

$library = Join-Path $jniRoot "$Abi\lib$LibName.so"
if (-not (Test-Path -LiteralPath $library)) {
    throw "没有找到 $library。请确认 Rust 包 `"$Package`" 的 [lib] name 为 `"$LibName`" 且 crate-type 包含 cdylib。"
}

if (-not $SkipGradle) {
    $task = if ($Profile -eq 'release') { 'assembleRelease' } else { 'assembleDebug' }
    $gradlew = Join-Path $platform 'gradlew.bat'
    & $gradlew -p $platform $task --max-workers=1 --no-daemon
    if ($LASTEXITCODE -ne 0) {
        throw "Gradle 失败，退出码 $LASTEXITCODE。"
    }
}

$apkDirectory = Join-Path $platform "app\build\outputs\apk\$Profile"
$apk = Get-ChildItem -LiteralPath $apkDirectory -Filter '*.apk' -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
if ($apk) {
    $sizeMiB = [Math]::Round($apk.Length / 1MB, 1)
    Write-Host "APK         : $($apk.FullName) ($sizeMiB MiB)"
    Write-Host '未安装到设备，也未发布。'
}
else {
    Write-Warning "没有在 $apkDirectory 找到 APK。"
}
