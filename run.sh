#!/bin/zsh
set -exu -o pipefail
#rust-lldb  ./target/aarch64-apple-darwin/release/tikv-server  -- --pd-endpoints="127.0.0.1:2379" \
RUST_BACKTRACE=full ./target/debug/tikv-server  --pd-endpoints="127.0.0.1:2379" \
                --addr="127.0.0.1:20160" \
                --data-dir=tikv1  \
                -C tikv-enc.yaml
                #--log-file=tikv1.log
