#!/bin/sh
# C compiler for `cargo clippy/check --target x86_64-unknown-linux-musl` (ring, via ureq/rustls,
# has C code; plain cargo has no musl C compiler, cargo-zigbuild only covers builds):
#   CC_x86_64_unknown_linux_musl=scripts/zigcc-x86_64-musl.sh AR_x86_64_unknown_linux_musl="zig ar" cargo clippy ...
# Drops cc-rs's Rust-style --target= (zig takes -target).
for a; do shift; case "$a" in --target=*) ;; *) set -- "$@" "$a";; esac; done
exec zig cc -target x86_64-linux-musl "$@"
