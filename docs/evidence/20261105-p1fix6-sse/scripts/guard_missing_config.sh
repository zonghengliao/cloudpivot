#!/usr/bin/env bash
# P1-FIX-6 首启自检实证：**把白名单删掉**，启动日志必须点名告警（不是静默 401）。
#
# 做法：把 backend/config 复制到临时目录，删掉 `[middleware.auth].query_token_paths` 那一行，
# 用同一份库在 18141 端口起一个一次性实例，抓启动日志里的两条告警：
#   - `nik_web::app`：白名单为空 → 浏览器 EventSource 类端点将 401（装配期告警）
#   - `cloudpivot::bootstrap`：SSE 鉴权自检未通过（业务侧告警，带端点名与两种修法）
# 然后收工，不留进程。
set -uo pipefail
ROOT=${ROOT:-/opt/cloudpivot}
BIN=${BIN:-/root/p1fix5c-target/debug/cloudpivot}
RUN=${RUN:-/tmp/p1fix6-iso/guard-missing}
DB=${DB:-cloudpivot_sse6}
PORT=${PORT:-18141}
DB_USER=${DB_USER:-e2erole}
DB_PASS=${DB_PASS:-e2eiso_2026}
PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }

rm -rf "$RUN"; mkdir -p "$RUN/config"
cp "$ROOT/backend/config/config.toml" "$RUN/config/config.toml"
cp "$ROOT/backend/config/config-test.toml" "$RUN/config/config-test.toml" 2>/dev/null || true
# 删掉白名单（含它上面的注释块，模拟「这个环境从没配过」）
python3 - "$RUN/config/config.toml" <<'PY'
import re, sys
p = sys.argv[1]
text = open(p, encoding='utf-8').read()
text2, n = re.subn(r'^query_token_paths\s*=.*\n', '', text, flags=re.M)
open(p, 'w', encoding='utf-8').write(text2)
print(f'已从临时 config.toml 删除 query_token_paths 行：{n} 行')
PY
grep -q '^query_token_paths' "$RUN/config/config.toml" && bad "临时配置里还留着白名单" || ok "临时配置已不含 query_token_paths（模拟漏配环境）"

# 应用是从 **CWD** 解析相对路径的（`[database] path = "migrations"`、uploads/downloads/templates/static），
# 所以临时目录里必须把这些入口挂回去，否则没走到自检就先 FATAL（实测：
# `FATAL: 配置错误: 数据库迁移失败: read migrations dir 'migrations': No such file or directory`）。
for d in migrations uploads downloads templates static; do
  [[ -e "$ROOT/backend/$d" ]] && ln -sfn "$ROOT/backend/$d" "$RUN/$d"
done

(
  cd "$RUN" || exit 1
  export NIK_PROFILE=test
  export NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432
  export NIK_DATABASE_NAME="$DB" NIK_DATABASE_USER="$DB_USER" NIK_DATABASE_PASSWORD="$DB_PASS"
  export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
  export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
  export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
  exec "$BIN"
) > "$RUN/startup.log" 2>&1 &
PID=$!
for _ in $(seq 1 60); do
  curl -fsS -m 2 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break
  kill -0 "$PID" 2>/dev/null || break
  sleep 1
done
CODE=$(curl -s -o /dev/null -w '%{http_code}' -m 3 "http://127.0.0.1:$PORT/health" || true)
echo "  启动结果：/health=$CODE（pid=$PID）"
kill -TERM "$PID" 2>/dev/null; wait "$PID" 2>/dev/null

echo "  ---- 告警原文 ----"
grep -E "查询参数令牌白名单为空|SSE 鉴权自检未通过" "$RUN/startup.log" | sed 's/^/  /' || true
N1=$(grep -c "查询参数令牌白名单为空" "$RUN/startup.log" || true)
N2=$(grep -c "SSE 鉴权自检未通过" "$RUN/startup.log" || true)
[[ "$N1" -ge 1 ]] && ok "装配期告警在（nik_web::app，缺配置不再静默）" || bad "装配期告警缺失"
[[ "$N2" -ge 1 ]] && ok "业务自检告警在（cloudpivot::bootstrap，点名端点与修法）" || bad "业务自检告警缺失"
grep -q "/api/business/notice/stream" "$RUN/startup.log" && ok "告警里点明了受影响端点" || bad "告警没提端点"
echo "  ---- 反证：该实例下 ?token= 应该 401（与告警描述一致） ----"
echo "  （实例已停，401 结论由 probe ③ 的「非白名单端点 401」同一机制覆盖）"
printf '=== 首启自检实证：PASS=%d FAIL=%d ===\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
