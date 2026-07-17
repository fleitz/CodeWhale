$ErrorActionPreference = "Stop"

function Assert-EnvironmentValue {
    param(
        [string]$Name,
        [string]$Expected
    )

    $actual = [Environment]::GetEnvironmentVariable($Name, "Process")
    if ($actual -cne $Expected) {
        throw "$Name mismatch.`nexpected: $Expected`nactual:   $actual"
    }
}

$fixture = [System.IO.Path]::Combine(
    [System.IO.Path]::GetTempPath(),
    "Codewhale OHOS Env $([Guid]::NewGuid())"
)

try {
    $directories = @(
        [System.IO.Path]::Combine($fixture, "llvm", "bin"),
        [System.IO.Path]::Combine($fixture, "sysroot"),
        [System.IO.Path]::Combine($fixture, "build", "cmake")
    )
    foreach ($directory in $directories) {
        New-Item -ItemType Directory -Path $directory -Force | Out-Null
    }

    $requiredFiles = @(
        [System.IO.Path]::Combine($fixture, "llvm", "bin", "clang.exe"),
        [System.IO.Path]::Combine($fixture, "llvm", "bin", "clang++.exe"),
        [System.IO.Path]::Combine($fixture, "llvm", "bin", "llvm-ar.exe"),
        [System.IO.Path]::Combine($fixture, "build", "cmake", "ohos.toolchain.cmake")
    )
    foreach ($file in $requiredFiles) {
        New-Item -ItemType File -Path $file -Force | Out-Null
    }

    $env:OHOS_NATIVE_SDK = $fixture
    . "$PSScriptRoot/ohos-env.ps1" | Out-Null

    $sdk = (Resolve-Path -LiteralPath $fixture).Path
    $target = "aarch64_unknown_linux_ohos"
    $clang = [System.IO.Path]::Combine($sdk, "llvm", "bin", "clang.exe")
    $clangxx = [System.IO.Path]::Combine($sdk, "llvm", "bin", "clang++.exe")
    $ar = [System.IO.Path]::Combine($sdk, "llvm", "bin", "llvm-ar.exe")
    $sysroot = [System.IO.Path]::Combine($sdk, "sysroot")
    $cmakeToolchain = [System.IO.Path]::Combine(
        $sdk,
        "build",
        "cmake",
        "ohos.toolchain.cmake"
    )
    $commonFlags = "-target aarch64-linux-ohos --sysroot=`"$sysroot`" -D__MUSL__"
    $separator = [char]0x1f
    $encodedRustflags = @(
        "-Clink-arg=-target",
        "-Clink-arg=aarch64-linux-ohos",
        "-Clink-arg=--sysroot=$sysroot",
        "-Clink-arg=-D__MUSL__"
    ) -join $separator

    Assert-EnvironmentValue "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER" $clang
    Assert-EnvironmentValue "AR_$target" $ar
    Assert-EnvironmentValue "CC_$target" $clang
    Assert-EnvironmentValue "CXX_$target" $clangxx
    Assert-EnvironmentValue "CFLAGS_$target" $commonFlags
    Assert-EnvironmentValue "CXXFLAGS_$target" $commonFlags
    Assert-EnvironmentValue "BINDGEN_EXTRA_CLANG_ARGS_$target" $commonFlags
    Assert-EnvironmentValue "CMAKE_TOOLCHAIN_FILE_$target" $cmakeToolchain
    Assert-EnvironmentValue "CC_SHELL_ESCAPED_FLAGS" "1"
    Assert-EnvironmentValue "CARGO_ENCODED_RUSTFLAGS" $encodedRustflags
}
finally {
    if (Test-Path -LiteralPath $fixture) {
        Remove-Item -LiteralPath $fixture -Recurse -Force
    }
}

Write-Host "ohos-env.ps1 tests passed"
