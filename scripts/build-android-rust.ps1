param(
    [ValidateSet("debug", "release")]
    [string]$Profile = "release",
    [string[]]$Targets = @("aarch64-linux-android", "x86_64-linux-android"),
    [string]$TargetDir = ""
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $TargetDir) {
    $TargetDir = Join-Path $repoRoot "target\android-so"
}
$ndkRoot = "C:\Users\kao24\AppData\Local\Android\Sdk\ndk\28.2.13676358"
$ndkBin = Join-Path $ndkRoot "toolchains\llvm\prebuilt\windows-x86_64\bin"

if (-not (Test-Path $ndkBin)) {
    throw "Android NDK toolchain not found at $ndkBin"
}

$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
$env:CARGO_TARGET_DIR = $TargetDir

$gitSha = (& git -C $repoRoot rev-parse --short=7 HEAD 2>$null).Trim()
if ($gitSha) {
    $env:VELOXDESK_GIT_SHA = $gitSha
}

$abiMap = @{
    "aarch64-linux-android" = @{ Abi = "arm64-v8a"; Clang = "aarch64-linux-android26-clang.cmd"; EnvTarget = "aarch64_linux_android"; LinkerEnv = "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER" }
    "x86_64-linux-android"  = @{ Abi = "x86_64";    Clang = "x86_64-linux-android26-clang.cmd";  EnvTarget = "x86_64_linux_android";  LinkerEnv = "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER" }
}

$jniLibsRoot = Join-Path $repoRoot "apps\android\app\src\main\jniLibs"

foreach ($t in $Targets) {
    if (-not $abiMap.ContainsKey($t)) {
        throw "Unsupported Android target: $t"
    }
    $cfg = $abiMap[$t]
    $clang = Join-Path $ndkBin $cfg.Clang
    $ar = Join-Path $ndkBin "llvm-ar.exe"

    Set-Item -Path "Env:CC_$($cfg.EnvTarget)" -Value $clang
    Set-Item -Path "Env:AR_$($cfg.EnvTarget)" -Value $ar
    Set-Item -Path "Env:$($cfg.LinkerEnv)" -Value $clang

    $cargoArgs = @("build", "--manifest-path", (Join-Path $repoRoot "Cargo.toml"), "-p", "veloxdesk-android-bridge", "--target", $t)
    if ($Profile -eq "release") {
        $cargoArgs += "--release"
    }

    Write-Host "Building veloxdesk-android-bridge ($Profile) for $t ($($cfg.Abi))..."
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed for $t with exit code $LASTEXITCODE"
    }

    $soSrc = Join-Path $TargetDir "$t\$Profile\libveloxdesk_android.so"
    if (-not (Test-Path $soSrc)) {
        throw "Expected .so not found at $soSrc"
    }

    $dstDir = Join-Path $jniLibsRoot $cfg.Abi
    New-Item -ItemType Directory -Force -Path $dstDir | Out-Null
    Copy-Item -Force $soSrc (Join-Path $dstDir "libveloxdesk_android.so")
    $sizeMb = [Math]::Round((Get-Item (Join-Path $dstDir "libveloxdesk_android.so")).Length / 1MB, 2)
    Write-Host "Copied -> $dstDir\libveloxdesk_android.so ($sizeMb MiB)"
}

Write-Host "Done."
