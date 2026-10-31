@echo off
rem Tests for scripts\mux-agent.cmd, the Windows entry shim. Run: tests\shim.cmd [stub.exe]
rem The fake binary is a stub .exe that prints the first line of label.txt beside it, then each argument as [arg], and
rem exits with SHIM_EXIT (0 when unset). Pass a prebuilt one, or the test builds it with the .NET Framework csc.exe.
rem No labels or goto: the repo checks this file out with LF endings, where label search is unreliable.
setlocal
set "REPO=%~dp0.."
set "T=%TEMP%\mux shim %RANDOM%"
set "G=%T%\got.txt"
set "W=%T%\want.txt"
set FAIL=0
set "SHIM_EXIT="
set "MUX_AGENTS_BIN="
mkdir "%T%"

set "STUB=%~1"
if not defined STUB set "STUB=%T%\stub.exe"
if not exist "%STUB%" (
  >"%T%\stub.cs" echo using System;using System.IO;
  >>"%T%\stub.cs" echo class S{static int Main^(string[] a^){
  >>"%T%\stub.cs" echo Console.WriteLine^(File.ReadAllLines^(AppDomain.CurrentDomain.BaseDirectory+"label.txt"^)[0].Trim^(^)^);
  >>"%T%\stub.cs" echo foreach^(string x in a^)Console.WriteLine^("["+x+"]"^);
  >>"%T%\stub.cs" echo string e=Environment.GetEnvironmentVariable^("SHIM_EXIT"^);
  >>"%T%\stub.cs" echo return string.IsNullOrEmpty^(e^)?0:int.Parse^(e^);}}
  "%WINDIR%\Microsoft.NET\Framework64\v4.0.30319\csc.exe" /nologo /out:"%STUB%" "%T%\stub.cs" >nul
)
if not exist "%STUB%" (
  echo FAIL no stub binary
  exit /b 1
)

set "ARCH=%PROCESSOR_ARCHITECTURE%"
if defined PROCESSOR_ARCHITEW6432 set "ARCH=%PROCESSOR_ARCHITEW6432%"
set "OSARCH=windows-%ARCH%"
if /i "%ARCH%"=="AMD64" set "OSARCH=windows-x86_64"
if /i "%ARCH%"=="ARM64" set "OSARCH=windows-aarch64"

rem tier1_env_wins
set "R=%T%\c1\plug in"
mkdir "%R%\scripts" "%T%\c1\env bin"
copy /y "%REPO%\scripts\mux-agent.cmd" "%R%\scripts\" >nul
copy /y "%STUB%" "%T%\c1\env bin\mux-agent.exe" >nul
>"%T%\c1\env bin\label.txt" echo tier1
set "MUX_AGENTS_BIN=%T%\c1\env bin\mux-agent.exe"
call "%R%\scripts\mux-agent.cmd" list >"%G%"
set RC=%errorlevel%
set "MUX_AGENTS_BIN="
>"%W%" echo tier1
>>"%W%" echo [list]
set "OK="
if "%RC%"=="0" fc /b "%G%" "%W%" >nul && set OK=1
if defined OK (echo ok   tier1_env_wins) else (echo FAIL tier1_env_wins rc=%RC% & type "%G%" & set /a FAIL+=1)

rem tier2_bin_os_arch
set "R=%T%\c2\plug in"
mkdir "%R%\scripts\bin\%OSARCH%"
copy /y "%REPO%\scripts\mux-agent.cmd" "%R%\scripts\" >nul
copy /y "%STUB%" "%R%\scripts\bin\%OSARCH%\mux-agent.exe" >nul
>"%R%\scripts\bin\%OSARCH%\label.txt" echo tier2
call "%R%\scripts\mux-agent.cmd" list -All >"%G%"
set RC=%errorlevel%
>"%W%" echo tier2
>>"%W%" echo [list]
>>"%W%" echo [-All]
set "OK="
if "%RC%"=="0" fc /b "%G%" "%W%" >nul && set OK=1
if defined OK (echo ok   tier2_bin_os_arch) else (echo FAIL tier2_bin_os_arch rc=%RC% & type "%G%" & set /a FAIL+=1)

rem tier3_target_release
set "R=%T%\c3\plug in"
mkdir "%R%\scripts" "%R%\target\release"
copy /y "%REPO%\scripts\mux-agent.cmd" "%R%\scripts\" >nul
copy /y "%STUB%" "%R%\target\release\mux-agent.exe" >nul
>"%R%\target\release\label.txt" echo tier3
call "%R%\scripts\mux-agent.cmd" list >"%G%"
set RC=%errorlevel%
>"%W%" echo tier3
>>"%W%" echo [list]
set "OK="
if "%RC%"=="0" fc /b "%G%" "%W%" >nul && set OK=1
if defined OK (echo ok   tier3_target_release) else (echo FAIL tier3_target_release rc=%RC% & type "%G%" & set /a FAIL+=1)

rem precedence_1_2_3: all three, then without MUX_AGENTS_BIN, then without tier 2.
set "R=%T%\c4\plug in"
mkdir "%R%\scripts\bin\%OSARCH%" "%R%\target\release" "%T%\c4\env bin"
copy /y "%REPO%\scripts\mux-agent.cmd" "%R%\scripts\" >nul
copy /y "%STUB%" "%T%\c4\env bin\mux-agent.exe" >nul
>"%T%\c4\env bin\label.txt" echo tier1
copy /y "%STUB%" "%R%\scripts\bin\%OSARCH%\mux-agent.exe" >nul
>"%R%\scripts\bin\%OSARCH%\label.txt" echo tier2
copy /y "%STUB%" "%R%\target\release\mux-agent.exe" >nul
>"%R%\target\release\label.txt" echo tier3
set "MUX_AGENTS_BIN=%T%\c4\env bin\mux-agent.exe"
call "%R%\scripts\mux-agent.cmd" >"%G%"
set "MUX_AGENTS_BIN="
call "%R%\scripts\mux-agent.cmd" >>"%G%"
del "%R%\scripts\bin\%OSARCH%\mux-agent.exe"
call "%R%\scripts\mux-agent.cmd" >>"%G%"
set RC=%errorlevel%
>"%W%" echo tier1
>>"%W%" echo tier2
>>"%W%" echo tier3
set "OK="
if "%RC%"=="0" fc /b "%G%" "%W%" >nul && set OK=1
if defined OK (echo ok   precedence_1_2_3) else (echo FAIL precedence_1_2_3 rc=%RC% & type "%G%" & set /a FAIL+=1)

rem directory_skipped: a folder named in MUX_AGENTS_BIN is not a binary.
set "R=%T%\c5\plug in"
mkdir "%R%\scripts" "%R%\target\release" "%T%\c5\env bin\mux-agent.exe"
copy /y "%REPO%\scripts\mux-agent.cmd" "%R%\scripts\" >nul
copy /y "%STUB%" "%R%\target\release\mux-agent.exe" >nul
>"%R%\target\release\label.txt" echo tier3
set "MUX_AGENTS_BIN=%T%\c5\env bin\mux-agent.exe"
call "%R%\scripts\mux-agent.cmd" d >"%G%"
set RC=%errorlevel%
set "MUX_AGENTS_BIN="
>"%W%" echo tier3
>>"%W%" echo [d]
set "OK="
if "%RC%"=="0" fc /b "%G%" "%W%" >nul && set OK=1
if defined OK (echo ok   directory_skipped) else (echo FAIL directory_skipped rc=%RC% & type "%G%" & set /a FAIL+=1)

rem spaces_in_root_and_args
set "R=%T%\c6\plug in"
mkdir "%R%\scripts" "%R%\target\release"
copy /y "%REPO%\scripts\mux-agent.cmd" "%R%\scripts\" >nul
copy /y "%STUB%" "%R%\target\release\mux-agent.exe" >nul
>"%R%\target\release\label.txt" echo tier3
call "%R%\scripts\mux-agent.cmd" send "a b" "" "it's" "x  y" >"%G%"
set RC=%errorlevel%
>"%W%" echo tier3
>>"%W%" echo [send]
>>"%W%" echo [a b]
>>"%W%" echo []
>>"%W%" echo [it's]
>>"%W%" echo [x  y]
set "OK="
if "%RC%"=="0" fc /b "%G%" "%W%" >nul && set OK=1
if defined OK (echo ok   spaces_in_root_and_args) else (echo FAIL spaces_in_root_and_args rc=%RC% & type "%G%" & set /a FAIL+=1)

rem exit_code_forwarded
set "R=%T%\c7\plug in"
mkdir "%R%\scripts" "%R%\target\release"
copy /y "%REPO%\scripts\mux-agent.cmd" "%R%\scripts\" >nul
copy /y "%STUB%" "%R%\target\release\mux-agent.exe" >nul
>"%R%\target\release\label.txt" echo tier3
set SHIM_EXIT=7
call "%R%\scripts\mux-agent.cmd" stop z >"%G%"
set RC=%errorlevel%
set "SHIM_EXIT="
>"%W%" echo tier3
>>"%W%" echo [stop]
>>"%W%" echo [z]
set "OK="
if "%RC%"=="7" fc /b "%G%" "%W%" >nul && set OK=1
if defined OK (echo ok   exit_code_forwarded) else (echo FAIL exit_code_forwarded rc=%RC% & type "%G%" & set /a FAIL+=1)

rem none_usable_prints_entry_json_exit_1
set "R=%T%\c8\plug in"
mkdir "%R%\scripts"
copy /y "%REPO%\scripts\mux-agent.cmd" "%R%\scripts\" >nul
set "MUX_AGENTS_BIN=%T%\c8\missing.exe"
call "%R%\scripts\mux-agent.cmd" list >"%G%"
set RC=%errorlevel%
set "MUX_AGENTS_BIN="
>"%W%" echo {"ok":false,"verb":null,"code":1,"check":"entry","message":"no usable mux-agent binary for %OSARCH%"}
set "OK="
if "%RC%"=="1" fc /b "%G%" "%W%" >nul && set OK=1
if defined OK (echo ok   none_usable_prints_entry_json_exit_1) else (echo FAIL none_usable_prints_entry_json_exit_1 rc=%RC% & type "%G%" & set /a FAIL+=1)

rmdir /s /q "%T%"
echo %FAIL% failed
if %FAIL% gtr 0 exit /b 1
exit /b 0
