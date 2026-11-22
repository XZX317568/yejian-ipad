@echo off
set YEJIAN_PARAMS=D:\Documents\aidoku\rust-reader\variants\r3-c-calm.json
start "" "D:\Documents\aidoku\rust-reader\target\debug\yejian.exe" --data-dir "%~dp0data\library"
