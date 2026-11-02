#!/usr/bin/env bash
# D2：V0205 的「第 1 层防护」真库取证（psql 直连，绕过应用层）。
#
#   bash scripts/verify/d2_psql_evidence.sh
#
# 证明三件事：
#   ① 动作码不在白名单 → CHECK 拒绝（第 1 层，防绕过 API）；
#   ② 参数里出现 SQL 类键 → CHECK 拒绝（**禁配 SQL** 的数据库层兜底）；
#   ③ 参数不是 JSON 对象 / 同一触发点同动作重复 → 拒绝；
#   ④ 合法行能写进去、能查出来（第 1 步「配置能存能查」）。
#
# 所有写入都在末尾被删掉（不留夹具）。
set -uo pipefail

DB=${D2_DB:-cloudpivot_d2}
export PGPASSWORD=alun
PSQL="psql -h 127.0.0.1 -U alun -d $DB -At -v ON_ERROR_STOP=0"

PASS=0
FAIL=0

show() { # show <label> <sql> —— 打印 psql 原始输出（含 ERROR 原文）
  local label=$1
  local sql=$2
  echo "----- $label"
  echo "SQL> $sql"
  $PSQL -c "$sql" 2>&1 | sed 's/^/  /'
}

expect_error() { # expect_error <label> <sql> <pattern>
  local label=$1
  local sql=$2
  local pattern=$3
  local out
  out=$($PSQL -c "$sql" 2>&1)
  if printf '%s' "$out" | grep -q "$pattern"; then
    PASS=$((PASS + 1))
    echo "[PASS] $label  —— $(printf '%s' "$out" | head -1)"
  else
    FAIL=$((FAIL + 1))
    echo "[FAIL] $label  —— 期望命中「$pattern」，实际：$out"
  fi
}

expect_value() { # expect_value <label> <sql> <expected>
  local label=$1
  local sql=$2
  local expected=$3
  local out
  out=$($PSQL -c "$sql" 2>&1 | head -1)
  if [ "$out" = "$expected" ]; then
    PASS=$((PASS + 1))
    echo "[PASS] $label  —— $out"
  else
    FAIL=$((FAIL + 1))
    echo "[FAIL] $label  —— 期望 [$expected] 实际 [$out]"
  fi
}

echo "== D2 白名单三层防护 · 数据库层取证（库 $DB）=="
echo

# 清场
$PSQL -c "DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'D2EV-%'" >/dev/null 2>&1

# ---------- ① 动作码白名单 ----------
expect_error "① 未知动作码被 ck_flow_linkage_action_code 拒绝" \
  "INSERT INTO biz_flow_linkage_rule (id, name, trigger_entity, trigger_status, action_code) VALUES (gen_random_uuid(), 'D2EV-bad-code', 'TASK', 'DOING', 'EXECUTE_SQL');" \
  "ck_flow_linkage_action_code"

# ---------- ② 禁配 SQL ----------
for key in sql statement query script exec; do
  expect_error "② 参数键 \`$key\` 被 ck_flow_linkage_no_sql_key 拒绝" \
    "INSERT INTO biz_flow_linkage_rule (id, name, trigger_entity, trigger_status, action_code, action_params) VALUES (gen_random_uuid(), 'D2EV-sql-$key', 'TASK', 'DOING', 'SEND_NOTICE', '{\"$key\": \"UPDATE biz_voucher SET amount = 0\"}'::jsonb);" \
    "ck_flow_linkage_no_sql_key"
done

# ---------- ③ 参数形态 / 唯一键 ----------
expect_error "③ 参数不是 JSON 对象被拒绝" \
  "INSERT INTO biz_flow_linkage_rule (id, name, trigger_entity, trigger_status, action_code, action_params) VALUES (gen_random_uuid(), 'D2EV-params-array', 'TASK', 'DOING', 'SEND_NOTICE', '[1,2,3]'::jsonb);" \
  "ck_flow_linkage_params_object"

expect_value "④ 合法行能写入" \
  "INSERT INTO biz_flow_linkage_rule (id, name, trigger_entity, trigger_status, action_code, action_params, seq) VALUES (gen_random_uuid(), 'D2EV-ok', 'TASK', 'DOING', 'SEND_NOTICE', '{\"mode\": \"SHADOW\"}'::jsonb, 7) RETURNING name;" \
  "D2EV-ok"

expect_error "⑤ 同一触发点 + 同一动作重复写入被 uk_flow_linkage_rule 拒绝" \
  "INSERT INTO biz_flow_linkage_rule (id, name, trigger_entity, trigger_status, action_code, action_params) VALUES (gen_random_uuid(), 'D2EV-dup', 'TASK', 'DOING', 'SEND_NOTICE', '{}'::jsonb);" \
  "uk_flow_linkage_rule"

expect_value "⑥ 写入的规则能查回来（能存能查）" \
  "SELECT action_code || '/' || (action_params->>'mode') || '/seq=' || seq FROM biz_flow_linkage_rule WHERE name = 'D2EV-ok';" \
  "SEND_NOTICE/SHADOW/seq=7"

# ---------- 收尾 ----------
$PSQL -c "DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'D2EV-%'" >/dev/null
expect_value "⑦ 收尾：夹具已清空" \
  "SELECT count(*) FROM biz_flow_linkage_rule WHERE name LIKE 'D2EV-%';" \
  "0"

echo
echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
