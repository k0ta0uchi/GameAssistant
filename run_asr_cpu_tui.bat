@echo off
chcp 65001 > nul
setlocal
cd /d "%~dp0"

echo ======================================================================
echo   GameAssistant - CPU Whisper ASR Realtime Tester (TUI)
echo ======================================================================
echo.

if not exist "venv\Scripts\python.exe" (
    echo [ERROR] Python virtual environment was not found at venv\Scripts\python.exe.
    echo Please make sure the venv is set up properly.
    echo.
    pause
    exit /b 1
)

if not exist "models\kotoba-whisper-v2.0-faster" (
    echo [ERROR] Kotoba-Whisper model not found at models\kotoba-whisper-v2.0-faster.
    echo Please run GameAssistant Models Manager to download the model first.
    echo.
    pause
    exit /b 1
)

.\venv\Scripts\python.exe scripts\asr_cpu_tui.py %*

if %ERRORLEVEL% neq 0 (
    echo.
    echo [Exit] Process finished with code %ERRORLEVEL%.
    pause
)
