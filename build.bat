SETLOCAL
SET PATH=C:\Program Files\Git\bin;C:\Program Files\Git;%PATH%
cargo build --release
del Cargo.lock
ENDLOCAL
pause
