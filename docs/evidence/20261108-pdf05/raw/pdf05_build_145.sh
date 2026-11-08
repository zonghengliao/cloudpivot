#!/usr/bin/env bash
# PDF-05：把 145 上的隔离应用目录更新到 cb52503 并编译
# 只动 /root/pdf04-app 目录内容（PDF-04 留下的隔离 app），不碰 /opt/cloudpivot 服务、不碰 144
set -eu
export PATH=/root/.cargo/bin:$PATH
cd /root/pdf04-app
git fetch -q origin
git checkout -q cb52503
git log --oneline -1
cd backend
cargo build --bin cloudpivot -j 2 2>&1 | tail -15
echo BUILD_DONE
md5sum target/debug/cloudpivot
