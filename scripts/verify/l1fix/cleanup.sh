#!/bin/bash
# L1 修复验收 —— 测试数据清理（AI 用户 / 任务 / kanban card / 令牌 / 附件文件 / 模板 profile）
set -u
HB=/root/.hermes/hermes-agent/hermes
export HERMES_HOME=/root/.hermes
CARD=t_c319bc13
PROFILE=fix-check-ai
TMPL=l1fix-tmpl

echo "=== 0. 清理前基线 ==="
env HERMES_HOME=/root/.hermes $HB kanban --board dev ls 2>&1 | tail -4
ls -d /root/.hermes/profiles/fix-check-ai /root/.hermes/profiles/l1fix-tmpl /root/.hermes/workspace/fix-check-ai 2>/dev/null
ls -l /opt/cloudpivot/backend/uploads/ 2>/dev/null | tail -5

echo
echo "=== 1. 通过接口删 AI 用户（会备份 SOUL → 删 profile → 删 workspace → 软删账号）==="
set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
python3 - <<'PY'
import json, os, sys
sys.path.insert(0, "/tmp/l1fix")
from e2e_a import call, login
state = json.load(open("/tmp/l1fix/state.json"))
token = login()
st, r = call("DELETE", f"/api/system/user/ai/{state['user_id']}", token=token)
print(f"  DELETE /api/system/user/ai/{state['user_id']} -> HTTP {st} code={r.get('code')} msg={r.get('msg')}")
print(f"  data = {json.dumps(r.get('data'), ensure_ascii=False)}")
PY

echo
echo "=== 2. 删 kanban card（archive → purge）==="
env HERMES_HOME=/root/.hermes $HB kanban --board dev archive "$CARD" 2>&1 | tail -3
env HERMES_HOME=/root/.hermes $HB kanban --board dev archive --rm "$CARD" 2>&1 | tail -3
env HERMES_HOME=/root/.hermes $HB kanban --board dev ls 2>&1 | tail -3

echo
echo "=== 3. 删临时模板 profile $TMPL ==="
env HERMES_HOME=/root/.hermes $HB profile delete "$TMPL" -y 2>&1 | tail -2
ls -d /root/.hermes/profiles/$TMPL 2>/dev/null && echo "  [FAIL] 仍存在" || echo "  [OK] 已删除"

echo
echo "=== 4. 删附件上传文件 + SOUL 备份 ==="
python3 - <<'PY'
import glob, os
for path in glob.glob("/opt/cloudpivot/backend/uploads/01a18739-*"):
    os.remove(path)
    print(f"  已删附件文件 {path}")
for path in glob.glob("/root/.hermes/aiagent-soul-backups/fix-check-ai-*"):
    os.remove(path)
    print(f"  已删 SOUL 备份 {path}")
PY

echo
echo "=== 5. 清理后核对 ==="
echo "  profile 目录："; ls -d /root/.hermes/profiles/fix-check-ai 2>/dev/null || echo "    不存在 ✓"
echo "  workspace 目录："; ls -d /root/.hermes/workspace/fix-check-ai 2>/dev/null || echo "    不存在 ✓"
echo "  uploads 残留："; ls /opt/cloudpivot/backend/uploads/01a18739-* 2>/dev/null || echo "    无 ✓"
echo "  SOUL 备份残留："; ls /root/.hermes/aiagent-soul-backups/fix-check-ai-* 2>/dev/null || echo "    无 ✓"
export PGPASSWORD="$ALUN_DATABASE_PASSWORD"
echo "  库内（临时库 cloudpivot_l1fix）："
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_l1fix -Atc \
  "SELECT 'sys_user有效行=' || count(*) FROM sys_user WHERE username LIKE 'fix-%' AND deleted_at IS NULL" 
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_l1fix -Atc \
  "SELECT 'sys_user软删行=' || count(*) FROM sys_user WHERE username LIKE 'fix-%'"
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_l1fix -Atc \
  "SELECT 'biz_task行=' || count(*) FROM biz_task"
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_l1fix -Atc \
  "SELECT 'ai_token行=' || count(*) FROM biz_task_ai_token"
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_l1fix -Atc \
  "SELECT 'biz_task_attachment行=' || count(*) FROM biz_task_attachment WHERE deleted_at IS NULL"
