@echo off
setlocal
echo =========================================
echo   NUP Extractor - Native Build ^& Test
echo =========================================

where cargo >nul 2>&1
if errorlevel 1 (
    echo [ERROR] Native toolchain ^(cargo^) not found in PATH.
    exit /b 1
)

echo [1/4] Checking code formatting...
cargo fmt --all -- --check
if errorlevel 1 exit /b 1

echo [2/4] Running Clippy lints...
cargo clippy --workspace --all-targets -- -D warnings
if errorlevel 1 exit /b 1

echo [3/4] Running workspace test suite...
cargo test --workspace
if errorlevel 1 exit /b 1

echo [4/4] Building standalone release executable...
cargo build --release -p nup-extractor
if errorlevel 1 exit /b 1

copy /Y "target\release\NUP_Extractor.exe" "NUP_Extractor.exe" >nul
echo.
echo [SUCCESS] Built standalone executable: NUP_Extractor.exe
exit /b 0
