@echo off
chcp 65001 >nul
cd /d "%~dp0"
set YEJIAN_PARAMS=%~dp0params-随意调.json
start "" "D:\Documents\aidoku\rust-reader\target\debug\yejian.exe" --data-dir "%~dp0data\library"
