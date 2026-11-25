@echo off
title Build Native NUP Extractor
echo ========================================================
echo   RUNNING AUTOMATED TEST SUITE (TDD / CI)
echo ========================================================
dotnet test tests_native -c Release
if %ERRORLEVEL% neq 0 (
    echo.
    echo [!] Automated tests failed! Build aborted.
    pause
    exit /b %ERRORLEVEL%
)

echo.
echo ========================================================
echo   ALL TESTS PASSED! COMPILING STANDALONE EXECUTABLE
echo ========================================================
dotnet publish src_native -c Release -r win-x64 --self-contained -p:PublishSingleFile=true -p:IncludeNativeLibrariesForSelfExtract=true -p:EnableCompressionInSingleFile=true -o dist_native
if %ERRORLEVEL% equ 0 (
    copy /Y dist_native\NupExtractorApp.exe NUP_Extractor.exe
    rmdir /S /Q dist_native
    echo.
    echo ========================================================
    echo   BUILD SUCCESSFUL! Verified and updated NUP_Extractor.exe
    echo ========================================================
) else (
    echo.
    echo [!] Build failed!
)
pause
