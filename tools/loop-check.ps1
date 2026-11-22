<#
.SYNOPSIS
  每轮收尾的唯一判官：环境闸门 + 锁 + 4 条收尾断言 + STATE 证据核对。
#>
param(
  [switch]$Full,
  [string]$AcceptanceOut = ''
)

$repo = Split-Path -Parent $PSScriptRoot
$rust = Join-Path $repo 'rust-reader'
$logDir = Join-Path $repo '.tools\loop-logs'
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$log = Join-Path $logDir ("loop-check-" + $stamp + ".log")
$fail = New-Object System.Collections.Generic.List[string]

function Say([string]$text) {
  Write-Host $text
  Add-Content -Path $log -Value $text
}
function Ok([string]$text) { Say ("  PASS  " + $text) }
function Bad([string]$text) { Say ("  FAIL  " + $text); $fail.Add($text) }

Say ("=== loop-check " + $stamp + " ===")
Say ("repo: " + $repo)

Say '[环境闸门]'
$drive = $repo.Substring(0,1)
$disk = Get-PSDrive -Name $drive -ErrorAction SilentlyContinue
if ($disk) {
  $freeGB = [math]::Round($disk.Free / 1GB, 1)
  if ($freeGB -lt 30) { Bad ("可用磁盘 " + $freeGB + "GB < 30GB") } else { Ok ("可用磁盘 " + $freeGB + "GB") }
}
$os = Get-CimInstance Win32_OperatingSystem
$freeRam = [math]::Round($os.FreePhysicalMemory / 1MB, 1)
if ($freeRam -lt 3) { Bad ("可用内存 " + $freeRam + "GB < 3GB") } else { Ok ("可用内存 " + $freeRam + "GB") }
$tmp = Join-Path $repo '.tools\loop-tmp'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
Ok ("TMP 规定为 " + $tmp)

Say '[单写者锁]'
$lock = Join-Path $repo '.tools\loop.lock'
if (Test-Path $lock) {
  $owner = Get-Content $lock -Raw
  $pidMatch = [regex]::Match($owner, '(?m)^pid=(\d+)')
  $alive = $false
  if ($pidMatch.Success) { $alive = [bool](Get-Process -Id ([int]$pidMatch.Groups[1].Value) -ErrorAction SilentlyContinue) }
  if ($alive) { Bad ("有别的写者持有锁：" + $owner.Trim()) } else { Ok '锁文件存在但持有进程已死，可忽略' }
} else { Ok '无锁（当前无其他写者）' }

Say '[断言 1] git status'
$dirty = git -C $repo status --porcelain
if ($dirty) { Bad ("工作树不干净：" + ($dirty -join ' / ')) } else { Ok '工作树干净' }

Say '[断言 2] git log -1'
$last = git -C $repo log --oneline -1
if ($last) { Ok $last } else { Bad '没有提交记录' }

Say '[断言 3] 测试退出码'
$testLog = Join-Path $logDir ("test-" + $stamp + ".log")
$env:TMP = $tmp
$env:TEMP = $tmp
Push-Location $rust
try { cargo test --locked --workspace -j 1 -- --test-threads=1 *>&1 | Tee-Object -FilePath $testLog | Select-Object -Last 3 }
finally { Pop-Location }
if ($LASTEXITCODE -eq 0) { Ok ("测试通过（日志 " + $testLog + "）") } else { Bad ("测试失败 exit=" + $LASTEXITCODE) }

Say '[STATE 证据核对]'
$state = Join-Path $repo 'STATE.md'
if (Test-Path $state) {
  $text = Get-Content $state -Raw
  $refs = [regex]::Matches($text, '`((?:\.tools|artifacts)/[^` ]+)`') | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
  if ($refs.Count -eq 0) { Ok 'STATE 未引用可核对路径' }
  foreach ($rel in $refs) {
    # 注意：$full 会和 switch 参数 $Full 冲突（PowerShell 不区分大小写），故用别的名字
    $evidencePath = Join-Path $repo $rel.TrimEnd('/')
    if (Test-Path $evidencePath) { Ok ("存在 " + $rel) } else { Bad ("STATE 引用了不存在的证据：" + $rel) }
  }
} else { Bad 'STATE.md 缺失' }

if ($Full) {
  Say '[断言 4] 离屏验收'
  $out = if ($AcceptanceOut) { $AcceptanceOut } else { Join-Path $repo ("artifacts\rust\loop-check\" + $stamp) }
  Push-Location $rust
  try { cargo run --locked -j 1 --features acceptance --bin acceptance -- $out *>&1 | Select-Object -Last 3 }
  finally { Pop-Location }
  if ($LASTEXITCODE -eq 0) { Ok ("验收通过 " + $out) } else { Bad ("验收失败 exit=" + $LASTEXITCODE) }
}

Say ''
if ($fail.Count -eq 0) { Say 'LOOP-CHECK PASS'; exit 0 }
Say ("LOOP-CHECK FAIL (" + $fail.Count + ")")
$fail | ForEach-Object { Say ("  - " + $_) }
exit 1
