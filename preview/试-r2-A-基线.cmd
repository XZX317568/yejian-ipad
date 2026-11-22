@echo off
set YEJIAN_PARAMS=D:\Documents\aidoku\rust-reader\variants\r2-a-base.json
start "" "D:\Documents\aidoku\rust-reader\target\debug\yejian.exe" --data-dir "%~dp0data\library"
