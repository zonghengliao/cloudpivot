#!/bin/bash
# CODE-02：145 上线后状态取证（只读）—— 版本 / 服务 / 健康 / 迁移 / 二进制时间戳
# 用法（在 145 上）：bash /tmp/code02_verify_145.sh
set -uo pipefail
cd /opt/cloudpivot || exit 1
echo "=== ① 部署版本（HEAD 必须是我的 CODE-02 提交，或其下游包含它的提交）"
git log --oneline -3
echo "=== ② 我的提交是否在 HEAD 的祖先里（含本卡 5 个文件）"
git log --oneline -1 246f68f -- backend/src/business/erp/seq.rs backend/src/business/erp/customer_handler.rs
echo "=== ③ systemd 单元"
systemctl is-active cloudpivot-test
echo "=== ④ 后端监听与健康"
ss -lntp 2>/dev/null | grep 8033 || echo "(8033 未监听)"
curl -s -o /dev/null -w "backend /health = %{http_code}\n" http://127.0.0.1:8033/health
curl -s -o /dev/null -w "nginx  /health = %{http_code}\n" http://127.0.0.1/health
echo "=== ⑤ 二进制时间戳（应晚于 10:29 的一次 build）"
ls -l backend/target/debug/cloudpivot
echo "=== ⑥ 最近一次启动日志：迁移条数 / 监听地址"
journalctl -u cloudpivot-test --since "-2h" --no-pager | grep -iE "数据库迁移完成|listen" | tail -4
