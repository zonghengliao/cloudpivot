#!/usr/bin/env bash
# ============================================================================
# P3-06 验收 ①：迁移 V0339 + 启动期冻结器（存量 APPROVAL 节点签名模式冻结）
#
# 做法：把 P2-03 的真实升级库 `cloudpivot_prt03` 整库克隆成 `cloudpivot_p306_mig`，
#       再用本卡构建的二进制**真启动**，让迁移器按升级路径（V0332 → V0339）跑一遍：
#   ① 迁移前 `approval_mode` 分布（按 草稿 / 已发布 两档分开数）；
#   ② 按 D2 口径预测「多少 → ALL / 多少 → ANY / 多少算不出来」；
#   ③ 首启：应用 N 个迁移（含 V0339）+ 启动期冻结器把**已发布版本**的快照/hash/行同改；
#      冻结结果与预测逐项相等，且 APPROVAL 不再有 NULL；
#   ④ 行与快照逐节点对账（`graph_snapshot->nodes[]->approval_mode` == 行的值）——
#      hash 是从「行重建的 canonical JSON」算的，两边值一致 + 代码走同一序列化即为一致；
#   ⑤ 重启：应用 0 个迁移 + 冻结器「无需处理」（幂等）；
#   ⑥ 重放 up.sql：`UPDATE 0`（真正可复跑）；RAISE NOTICE 打印统计；
#   ⑦ down.sql：只清草稿（已发布档不可一键反向，见 down 文件头）→ 再重放 up.sql 复原；
#   ⑧ 收尾停实例（库与日志留档备查）。
#
# 环境：库账号口令走 `PGPASSWORD`；三把密钥 + admin 口令在 `/root/p306/instance.env`（600，不入库）。
# 端口 18306 / 库 cloudpivot_p306_mig。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

DB=${DB:-cloudpivot_p306_mig}
SRC_DB=${SRC_DB:-cloudpivot_prt03}
PORT=${PORT:-18306}
ENVF=${ENVF:-/root/p306/instance.env}
BIN=${BIN:-/root/p306/bin/cloudpivot}
RUN_DIR=/opt/cloudpivot/backend
LOG=/root/p306/migrate_instance.log
PIDF=/root/p306/migrate.pid
OUT="$RAW/10_upgrade.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

say "================ P3-06 ① 迁移 V0339 + 启动期冻结（$SRC_DB → $DB / :$PORT）================"

stop_instance() {
  local pid
  pid=$(cat "$PIDF" 2>/dev/null) || return 0
  kill "$pid" 2>/dev/null || return 0
  for _ in $(seq 1 30); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 1
  done
  kill -9 "$pid" 2>/dev/null || true
}

start_instance() {
  local i pid
  ( cd "$RUN_DIR" && setsid env $(grep -v '^#' "$ENVF" | grep -v '^$' | tr '\n' ' ') \
      NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
      "$BIN" >"$LOG" 2>&1 </dev/null & )
  for i in $(seq 1 120); do
    pid=$(ss -ltnp 2>/dev/null | sed -n "s/.*127\.0\.0\.1:$PORT .*pid=\([0-9]*\).*/\1/p" | head -1)
    if [ -n "$pid" ]; then echo "$pid" >"$PIDF"; return 0; fi
    sleep 1
  done
  return 1
}

wait_http() {
  local i=0
  while [ "$i" -lt 180 ]; do
    if curl -s -o /dev/null "http://127.0.0.1:$PORT/health"; then return 0; fi
    sleep 1
    i=$((i + 1))
  done
  return 1
}

# 分布：状态|模式|节点数
dist() {
  q "$DB" "SELECT v.version_status || '|' || coalesce(n.approval_mode,'<NULL>') || '|' || count(*)
              FROM biz_flow_node n JOIN biz_flow_def_version v ON v.id = n.version_id
             WHERE n.node_type='APPROVAL'
             GROUP BY v.version_status, n.approval_mode ORDER BY 1"
}
null_of() { # null_of STATUS
  q "$DB" "SELECT count(*) FROM biz_flow_node n JOIN biz_flow_def_version v ON v.id = n.version_id
            WHERE n.node_type='APPROVAL' AND n.approval_mode IS NULL AND v.version_status='$1'"
}

stop_instance
say ""
say "--- 0) 从 $SRC_DB 克隆一份（含真实 PUBLISHED/ARCHIVED 审批节点）---"
su postgres -c "dropdb --if-exists --force $DB" >/dev/null 2>&1
su postgres -c "createdb -O alun -T $SRC_DB $DB" || { bad "克隆库失败"; summary; }
eq "克隆库 APPROVAL 节点总数与源库一致" \
  "$(q "$DB" "select count(*) from biz_flow_node where node_type='APPROVAL'")" \
  "$(q "$SRC_DB" "select count(*) from biz_flow_node where node_type='APPROVAL'")"

say ""
say "--- 1) 迁移前 approval_mode 分布（APPROVAL 节点，按版本状态分开）---"
BEFORE=$(dist)
printf '%s\n' "$BEFORE"
PRE_PUB_NULL=$(null_of PUBLISHED)
PRE_ARC_NULL=$(null_of ARCHIVED)
PRE_DRAFT_NULL=$(null_of DRAFT)
say "迁移前 NULL：PUBLISHED=$PRE_PUB_NULL / ARCHIVED=$PRE_ARC_NULL / DRAFT=$PRE_DRAFT_NULL"

say ""
say "--- 2) 按 D2 口径预测冻结结果（候选人数 ≥2 → ALL，其余 → ANY）---"
PRED=$(psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -tAf "$EV/scripts/predict_freeze.sql" | tr -d ' ')
say "预测（全部待冻结节点）：pred_all|pred_any|pred_unknown = $PRED"
PRED_ALL=${PRED%%|*}
REST=${PRED#*|}
PRED_ANY=${REST%%|*}
PRED_UNK=${REST#*|}
eq "预测三项之和 = 全部 NULL 节点数（一个不漏）" \
  "$((PRED_ALL + PRED_ANY + PRED_UNK))" "$((PRE_PUB_NULL + PRE_ARC_NULL + PRE_DRAFT_NULL))"

say ""
say "--- 3) 首启：迁移器按升级路径应用（V0332 → V0339）+ 启动期冻结器 ---"
start_instance || { bad "实例没起来"; tail -20 "$LOG"; summary; }
wait_http || { bad "实例 180s 内没起来"; tail -40 "$LOG"; summary; }
APP=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG" | tail -1)
say "首启迁移日志：${APP:-<未找到>}"
[ -n "$APP" ] && ok "首启确实跑了迁移（$APP）" || bad "首启没打印迁移行数"
eq "V0339 已登记" \
  "$(q "$DB" "select count(*) from schema_migrations where version='339'")" "1"
eq "迁移最大号 = 339" "$(q "$DB" "select max(version::int) from schema_migrations")" "339"

say ""
say "  --- 3.1) 冻结器日志（启动期，RUST_LOG=info）---"
grep -E "P3-06 签名模式冻结" "$LOG" || say "  <日志里没有冻结器输出？>"
contains "冻结器报告处理了已发布版本" "$(grep -c 'P3-06 签名模式冻结完成' "$LOG")" "1"

say ""
say "  --- 3.2) 冻结后 approval_mode 分布 ---"
AFTER=$(dist)
printf '%s\n' "$AFTER"
eq "冻结后 APPROVAL 的 NULL = 0（隐式语义全部显式化）" \
  "$(( $(null_of PUBLISHED) + $(null_of ARCHIVED) + $(null_of DRAFT) ))" "0"

ACT_ALL=$(( $(q "$DB" "select count(*) from biz_flow_node where node_type='APPROVAL' and approval_mode='ALL'") ))
ACT_ANY=$(( $(q "$DB" "select count(*) from biz_flow_node where node_type='APPROVAL' and approval_mode='ANY'") ))
PRE_ALL=$(( $(q "$SRC_DB" "select count(*) from biz_flow_node where node_type='APPROVAL' and approval_mode='ALL'") ))
PRE_ANY=$(( $(q "$SRC_DB" "select count(*) from biz_flow_node where node_type='APPROVAL' and approval_mode='ANY'") ))
eq "本次新冻结成 ALL 的节点数 = 预测 pred_all" "$((ACT_ALL - PRE_ALL))" "$PRED_ALL"
eq "本次新冻结成 ANY 的节点数 = 预测 pred_any + pred_unknown" "$((ACT_ANY - PRE_ANY))" "$((PRED_ANY + PRED_UNK))"
say "迁移 + 启动冻结统计：ALL=$ACT_ALL / ANY=$ACT_ANY / 算不出（按 ANY 落库）=$PRED_UNK"

say ""
say "  --- 3.3) 行 vs 快照逐节点对账（已发布/归档版本）---"
q "$DB" "SELECT v.version_status || ' | ' || count(*) || ' 个节点 | approval_mode 不一致的节点数 = '
                || count(*) FILTER (WHERE snap.mode IS DISTINCT FROM n.approval_mode)
           FROM biz_flow_node n
           JOIN biz_flow_def_version v ON v.id = n.version_id
           LEFT JOIN LATERAL (
                 SELECT elem->>'approval_mode' AS mode
                   FROM jsonb_array_elements(v.graph_snapshot -> 'nodes') AS elem
                  WHERE elem->>'key' = n.node_key
           ) AS snap ON TRUE
          WHERE n.node_type = 'APPROVAL' AND v.version_status IN ('PUBLISHED','ARCHIVED')
          GROUP BY 1 ORDER BY 1"
eq "已发布版本：行与快照的 approval_mode 逐节点一致（hash 自检不会报『行被改』）" \
  "$(q "$DB" "SELECT count(*) FROM biz_flow_node n
                JOIN biz_flow_def_version v ON v.id = n.version_id
                LEFT JOIN LATERAL (
                      SELECT elem->>'approval_mode' AS mode
                        FROM jsonb_array_elements(v.graph_snapshot -> 'nodes') AS elem
                       WHERE elem->>'key' = n.node_key
                ) AS snap ON TRUE
               WHERE n.node_type='APPROVAL' AND v.version_status IN ('PUBLISHED','ARCHIVED')
                 AND (snap.mode IS DISTINCT FROM n.approval_mode)")" "0"

say ""
say "  --- 3.4) 抽样（前 8 个被冻结的审批节点）---"
q "$DB" "select v.version_status || ' | ' || n.node_key || ' | ' || coalesce(n.assignee_type,'-')
                || ' | mode=' || coalesce(n.approval_mode,'<NULL>')
           FROM biz_flow_node n JOIN biz_flow_def_version v ON v.id = n.version_id
          WHERE n.node_type='APPROVAL'
          ORDER BY v.version_status, n.node_key LIMIT 8"

eq "RATIO 节点缺 approve_ratio = 0 个（ck_biz_flow_node_ratio 仍成立）" \
  "$(q "$DB" "select count(*) from biz_flow_node where approval_mode='RATIO' and approve_ratio is null")" "0"

say ""
say "--- 4) 重启：应用 0 个迁移 + 冻结器无需处理（幂等）---"
stop_instance
: >"$LOG"
start_instance || { bad "重启失败"; tail -20 "$LOG"; summary; }
wait_http || { bad "重启后没起来"; tail -30 "$LOG"; summary; }
APP2=$(grep -oE "应用 [0-9]+ 个迁移" "$LOG" | tail -1)
say "重启迁移日志：${APP2:-<未找到>}"
eq "重启应用 0 个迁移" "${APP2:-<未找到>}" "应用 0 个迁移"
eq "重启后分布不变" "$(dist)" "$AFTER"
eq "重启时冻结器没有活干（没有第二行『冻结完成』）" \
  "$(grep -c 'P3-06 签名模式冻结完成' "$LOG")" "0"

say ""
say "--- 5) 重放 up.sql：UPDATE 0（可复跑）---"
stop_instance
REPLAY=$(psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0339__approval_sign_mode_freeze.up.sql 2>&1)
printf '%s\n' "$REPLAY"
eq "重放时 UPDATE 改 0 行" "$(printf '%s\n' "$REPLAY" | grep -c '^UPDATE 0$')" "1"
eq "重放后分布不变" "$(dist)" "$AFTER"
contains "重放打印了统计 NOTICE" "$REPLAY" "V0339 草稿冻结"
contains "NOTICE 点名已发布档交给启动期冻结器" "$REPLAY" "flow::sign_freeze"

say ""
say "--- 6) down.sql：只清草稿档（已发布档不可一键反向，见 down 文件头）---"
DOWN=$(psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0339__approval_sign_mode_freeze.down.sql 2>&1)
printf '%s\n' "$DOWN"
eq "down 后草稿 APPROVAL 的 NULL 数 = 草稿节点总数" \
  "$(null_of DRAFT)" \
  "$(q "$DB" "select count(*) from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id
              where n.node_type='APPROVAL' and v.version_status='DRAFT'")"
eq "down 不动已发布档（冻结结果保持）" "$(null_of PUBLISHED)" "0"
contains "down 打印了被清行数" "$DOWN" "V0339 down：已把"

say ""
say "  --- 6.1) 再重放 up.sql：草稿档复原 ---"
psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0339__approval_sign_mode_freeze.up.sql >/dev/null 2>&1 \
  && ok "up 重放成功" || bad "up 重放失败"
eq "up 重放后的分布 = 首启后的分布" "$(dist)" "$AFTER"

say ""
say "--- 7) 收尾：停实例（库与日志留档备查）---"
stop_instance && ok "实例已停"
say ""
say "迁移前后对比："
say "  迁移前：$(printf '%s' "$BEFORE" | tr '\n' ' ')"
say "  迁移后：$(printf '%s' "$AFTER" | tr '\n' ' ')"
say "  预测：ALL=$PRED_ALL / ANY=$PRED_ANY / 算不出=$PRED_UNK"
summary
