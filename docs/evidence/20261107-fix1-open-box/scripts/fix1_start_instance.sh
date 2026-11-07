#!/bin/bash
# FIX-1 隔离实例启动器（145 测试机）
#
# 用法：start_instance.sh <env 文件绝对路径>
# 例：  start_instance.sh /root/fix1/env2
#
# 为什么不用 systemd：三个实例要**同时**跑（8034/8035/8036），且不能碰正在服役的
# cloudpivot-test（8033 / cloudpivot_dev）。单独进程 + 各自 env 文件最省事。
cd /opt/cloudpivot/backend || exit 1
set -a
# shellcheck disable=SC1090
. "$1"
set +a
exec ./target/debug/cloudpivot-fix1
