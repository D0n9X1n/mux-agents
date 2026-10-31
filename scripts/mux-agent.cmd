@echo off
rem mux-agent entry shim: runs the first usable binary of MUX_AGENTS_BIN, scripts\bin\windows-<arch>\mux-agent.exe and
rem target\release\mux-agent.exe, passing every argument and the exit code through. With none usable it prints an entry
rem refusal and exits 1. No labels or goto: the repo checks this file out with LF endings, where label search is unreliable.
setlocal
set "ARCH=%PROCESSOR_ARCHITECTURE%"
if defined PROCESSOR_ARCHITEW6432 set "ARCH=%PROCESSOR_ARCHITEW6432%"
set "OSARCH=windows-%ARCH%"
if /i "%ARCH%"=="AMD64" set "OSARCH=windows-x86_64"
if /i "%ARCH%"=="ARM64" set "OSARCH=windows-aarch64"
set "BIN="
if defined MUX_AGENTS_BIN if exist "%MUX_AGENTS_BIN%" if not exist "%MUX_AGENTS_BIN%\" set "BIN=%MUX_AGENTS_BIN%"
if not defined BIN if exist "%~dp0bin\%OSARCH%\mux-agent.exe" if not exist "%~dp0bin\%OSARCH%\mux-agent.exe\" set "BIN=%~dp0bin\%OSARCH%\mux-agent.exe"
if not defined BIN if exist "%~dp0..\target\release\mux-agent.exe" if not exist "%~dp0..\target\release\mux-agent.exe\" set "BIN=%~dp0..\target\release\mux-agent.exe"
if not defined BIN (
  echo {"ok":false,"verb":null,"code":1,"check":"entry","message":"no usable mux-agent binary for %OSARCH%"}
  exit /b 1
)
"%BIN%" %*
exit /b %errorlevel%
