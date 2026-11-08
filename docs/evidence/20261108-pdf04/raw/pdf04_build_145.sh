#!/usr/bin/env bash
# PDF-04：在 145 上把隔离应用目录更新到 5cb3bc8 并编译（只动 /root/pdf04-app，不碰 /opt/cloudpivot 服务）
set -eu
export PATH=/root/.cargo/bin:$PATH
cd /root
if [ ! -d pdf04-app ]; then
    git clone -q /opt/git/cloudpivot.git pdf04-app
fi
cd /root/pdf04-app
git fetch -q origin
git checkout -q 5cb3bc8
git log --oneline -1
cd backend
cargo build --bin cloudpivot 2>&1 | tail -20
echo BUILD_DONE
md5sum target/debug/cloudpivot
