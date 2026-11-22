# Keep Flutter's short options (notably -d) out of PowerShell's common
# parameter binder, where -d otherwise means -Debug.
$flutterArguments = $args
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$sdk = Join-Path $projectRoot '.tools\flutter-source\flutter-3.50.0'
if (-not (Test-Path -LiteralPath "$sdk\bin\flutter.bat")) {
    throw '项目 Flutter 3.50.0 尚未准备好。请按 BUILD.md 准备本地 SDK。'
}
$env:PUB_CACHE = Join-Path $projectRoot '.tools\pub-cache'
$env:GRADLE_USER_HOME = Join-Path $projectRoot '.tools\gradle'
$env:CARGO_BUILD_JOBS = '1'
$env:PUB_HOSTED_URL = 'https://pub.dev'
# Android builds and Dart tests do not need desktop plugin symlinks.
# Opt in for a Windows build on a machine with symlink support.
if (-not $env:YEJIAN_BUILD_WINDOWS) {
    $env:FLUTTER_WINDOWS = 'false'
    $env:FLUTTER_LINUX = 'false'
}
$env:PATH = "$sdk\bin;$env:PATH"

# Use the already configured Windows proxy for this process only.
$proxy = Get-ItemProperty -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings' -ErrorAction SilentlyContinue
if ($proxy.ProxyEnable -eq 1 -and $proxy.ProxyServer -match '^(127\.0\.0\.1|localhost):\d+$') {
    $env:HTTP_PROXY = "http://$($proxy.ProxyServer)"
    $env:HTTPS_PROXY = $env:HTTP_PROXY
    $env:GIT_CONFIG_COUNT = '1'
    $env:GIT_CONFIG_KEY_0 = 'http.proxy'
    $env:GIT_CONFIG_VALUE_0 = $env:HTTP_PROXY
}
& "$sdk\bin\flutter.bat" --no-version-check @flutterArguments
exit $LASTEXITCODE
