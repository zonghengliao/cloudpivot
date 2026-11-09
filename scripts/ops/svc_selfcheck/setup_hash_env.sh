#!/bin/bash
# ============================================================================
# setup_hash_env.sh —— 准备「能算 argon2id」的 python（只在需要它的机器上跑一次）
#
# 为什么需要：口令哈希必须和 `sys_user.admin` 现有形态一致
#   （`$argon2id$v=19$m=19456,t=2,p=1$…`，由 RustCrypto argon2 0.6 生成）。
#   系统 python 不保证带 argon2-cffi（实测：144/99 没有，145 有），
#   这里建一个专用 venv，装 argon2-cffi，**不碰系统 site-packages**。
#
# 用法：
#   bash scripts/ops/svc_selfcheck/setup_hash_env.sh [venv 路径]
#   默认 venv：/root/.venvs/svcchk
# 退出码：0 = 可用；1 = 装不上（网络/权限），此时改用已有 argon2 的机器生成凭据文件再拷过来
# ============================================================================
set -uo pipefail

VENV=${1:-/root/.venvs/svcchk}
say() { printf '%s\n' "$*"; }

if [ -x "$VENV/bin/python" ] && "$VENV/bin/python" -c 'import argon2' >/dev/null 2>&1; then
  say "✅ 已有可用 venv：$VENV（$("$VENV/bin/python" -c 'import importlib.metadata as m; print("argon2-cffi", m.version("argon2-cffi"))' 2>/dev/null)）"
  exit 0
fi

say "== 建 venv：$VENV"
python3 -m venv "$VENV" || { say "✗ venv 创建失败"; exit 1; }
say "== 装 argon2-cffi"
"$VENV/bin/pip" -q install --disable-pip-version-check argon2-cffi || { say "✗ 装不上 argon2-cffi（网络？）"; exit 1; }
if "$VENV/bin/python" -c 'import argon2' >/dev/null 2>&1; then
  say "✅ 就绪：$VENV"
  exit 0
fi
say "✗ 装完仍导入不了 argon2"
exit 1
