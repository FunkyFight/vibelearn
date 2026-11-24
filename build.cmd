@echo off
rem Builds the release executables for Windows and Linux into dist\, under the
rem names the updater looks for (see "Publishing a release" in README.md).
rem
rem Linux is cross-compiled from Windows with cargo-zigbuild. It needs, once:
rem   cargo install cargo-zigbuild
rem   winget install zig.zig
setlocal
cd /d "%~dp0"

set LINUX_TARGET=x86_64-unknown-linux-gnu
rem The oldest glibc the Linux executable has to run on.
set LINUX_GLIBC=2.28

where cargo-zigbuild >nul 2>&1
if errorlevel 1 (
    echo cargo-zigbuild is missing. Install it with: cargo install cargo-zigbuild
    exit /b 1
)

echo [1/3] Windows
cargo build --release
if errorlevel 1 exit /b 1

echo [2/3] Linux: %LINUX_TARGET%, glibc %LINUX_GLIBC%
rustup target add %LINUX_TARGET%
if errorlevel 1 exit /b 1
cargo zigbuild --release --target %LINUX_TARGET%.%LINUX_GLIBC%
if errorlevel 1 exit /b 1

echo [3/3] dist
if not exist dist mkdir dist
copy /y "target\release\vibelearn.exe" "dist\vibelearn-windows-x86_64.exe" >nul
if errorlevel 1 exit /b 1
copy /y "target\%LINUX_TARGET%\release\vibelearn" "dist\vibelearn-linux-x86_64" >nul
if errorlevel 1 exit /b 1

echo.
echo Done. Attach these to the GitHub release:
dir /b dist
endlocal
