@echo off
set YEJIAN_PARAMS=D:\Documents\aidoku\rust-reader\variants\r3-b-paper.json
start "" "D:\Documents\aidoku\rust-reader\target\debug\yejian.exe" --data-dir "%~dp0data\library"
