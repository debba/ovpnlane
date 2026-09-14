# Dot-source before invoking Cargo on Windows: . .\scripts\windows-env.ps1
# OpenVPN Core's Windows hardware-address helpers include this header even
# when external TUN is selected. No TAP/Wintun driver is installed or used.
$includeDirectory = Join-Path (Split-Path $PSScriptRoot -Parent) 'vendor\windows\include'
$env:CXXFLAGS = "$env:CXXFLAGS /EHsc /I`"$includeDirectory`""
$env:CMAKE_TOOLCHAIN_FILE = Join-Path $PSScriptRoot 'windows-msvc.cmake'
