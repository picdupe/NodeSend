@echo off
setlocal
rem Initialize a matching x64 compiler and SDK for this command only.
rem Override NODESEND_WINDOWS_SDK to use another installed SDK version.
if not defined NODESEND_WINDOWS_SDK set "NODESEND_WINDOWS_SDK=10.0.22621.0"
if not defined NODESEND_VSWHERE set "NODESEND_VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%NODESEND_VSWHERE%" set "NODESEND_VSWHERE=D:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%NODESEND_VSWHERE%" (
  echo Visual Studio Installer / vswhere.exe was not found. 1>&2
  exit /b 1
)
for /f "usebackq tokens=*" %%i in (`"%NODESEND_VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "NODESEND_VS=%%i"
if not defined NODESEND_VS (
  echo Visual Studio C++ build tools were not found. 1>&2
  exit /b 1
)
call "%NODESEND_VS%\VC\Auxiliary\Build\vcvarsall.bat" x64 %NODESEND_WINDOWS_SDK%
if errorlevel 1 exit /b 1
rem Tauri's winres helper checks RC explicitly instead of searching PATH.
set "RC=%WindowsSdkDir%bin\%WindowsSDKVersion%x64\rc.exe"
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
