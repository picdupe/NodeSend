@echo off
setlocal
rem Initialize a matching x64 compiler and SDK for this command only.
rem Override NODESEND_WINDOWS_SDK to use another installed SDK version.
if not defined NODESEND_VSWHERE set "NODESEND_VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%NODESEND_VSWHERE%" (
  echo Visual Studio Installer / vswhere.exe was not found. 1>&2
  exit /b 1
)
for /f "usebackq tokens=*" %%i in (`"%NODESEND_VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "NODESEND_VS=%%i"
if not defined NODESEND_VS (
  echo Visual Studio C++ build tools were not found. 1>&2
  exit /b 1
)
if defined NODESEND_WINDOWS_SDK (
  call "%NODESEND_VS%\VC\Auxiliary\Build\vcvarsall.bat" x64 %NODESEND_WINDOWS_SDK%
) else (
  call "%NODESEND_VS%\VC\Auxiliary\Build\vcvarsall.bat" x64
)
if errorlevel 1 exit /b 1
rem Tauri's winres helper checks RC explicitly instead of searching PATH.
if not defined RC for /f "delims=" %%i in ('where.exe rc.exe 2^>nul') do if not defined RC set "RC=%%i"
if not defined RC (
  echo Windows SDK rc.exe was not found. Add the installed SDK tools to PATH or set RC explicitly. 1>&2
  exit /b 1
)
set "RUSTC_LINKER=%NODESEND_VS%\VC\Tools\MSVC\%VCToolsVersion%\bin\Hostx64\x64\link.exe"
if not exist "%RC%" (
  echo Windows SDK rc.exe was not found at %RC%. 1>&2
  exit /b 1
)
if "%~1"=="" (
  echo Usage: scripts\msvc.cmd cargo test --manifest-path src-tauri/Cargo.toml
  exit /b 0
)
call %*
exit /b %errorlevel%
