@echo off
set YEJIAN_PARAMS=D:\Documents\aidoku\rust-reader\variants\r4-c-snappy.json
start "" "D:\Documents\aidoku\rust-reader\target\debug\yejian.exe" --data-dir "%~dp0data\library"
