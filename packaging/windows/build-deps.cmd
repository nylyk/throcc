@echo off
setlocal
set ROOT=%~dp0..\..
pip install meson==1.12.1 ninja==1.13.2 || exit /b 1
if not exist "%ROOT%\target\dav1d" git clone --depth 1 --branch 1.5.4 https://code.videolan.org/videolan/dav1d.git "%ROOT%\target\dav1d" || exit /b 1
meson setup "%ROOT%\target\dav1d\build" "%ROOT%\target\dav1d" --prefix="%ROOT%\deps" --default-library=shared --buildtype=release -Denable_tools=false -Denable_tests=false || exit /b 1
ninja -C "%ROOT%\target\dav1d\build" install || exit /b 1
