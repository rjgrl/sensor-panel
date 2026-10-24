@echo off
:: Compile only (no install) - writes the full compiler output to build_log.txt
cd /d "%~dp0"
echo Building... > build_log.txt
cargo build --release >> build_log.txt 2>&1
echo EXIT CODE %ERRORLEVEL% >> build_log.txt
