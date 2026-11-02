#!/usr/bin/env bash
# D3：`scripts/check.sh` 里「批次B 数据权限」闸②（登记表查询必须接 scope:: 或写 SCOPE-EXEMPT）
# 的**本卡文件**自查。
#
# 为什么不直接跑 check.sh：它的 clippy 步骤在本仓基线上本来就有红点（tests/testd/* 等历史文件），
# `set -e` 会让门禁那一步根本执行不到。所以这里按同一口径（同一个表清单 + 同样的注释剥离规则）
# 只检查本卡新增/改动的文件，并把全库基线数量作为参考打印出来。
set -uo pipefail

REPO=${REPO:-/opt/cloudpivot}
SRC="$REPO/backend/src"
BATCHB_SCOPED_TABLES="sys_user sys_dept sys_import_task sys_export_task sys_notice
sys_file operation_logs audit_logs auth_logs security_logs
biz_task biz_task_comment biz_task_attachment biz_task_worklog
biz_task_member biz_task_dependency biz_task_status_def
biz_project biz_project_member
biz_flow_def biz_flow_def_version biz_flow_node biz_flow_edge biz_flow_node_assignee
biz_flow_instance biz_flow_instance_node biz_flow_task biz_flow_cc"

MINE="$SRC/business/flow/linkage/actions.rs \
$SRC/business/flow/linkage/engine.rs \
$SRC/business/flow/linkage/spec.rs \
$SRC/business/flow/linkage/tests.rs \
$SRC/business/erp/voucher.rs \
$SRC/business/erp/stockin_query.rs \
$SRC/business/erp/stockin_handler.rs"

fail=0

echo "== 闸② 全库基线（仅供参考：存量未接入文件，非本卡范围）=="
base=0
for t in $BATCHB_SCOPED_TABLES; do
  for f in $(grep -rl "FROM $t" "$SRC" --include=*.rs 2>/dev/null || true); do
    code_lines=$(mktemp)
    grep -v -E '^[[:space:]]*(//|\*)' "$f" >"$code_lines" || true
    if grep -q "FROM $t" "$code_lines" \
      && ! grep -q "SCOPE-EXEMPT" "$f" \
      && ! grep -q "scope::" "$code_lines"; then
      base=$((base + 1))
    fi
    rm -f "$code_lines"
  done
done
echo "  全库未接入 file×table 计数：$base"
echo

echo "== 闸② 本卡文件（任一 FAIL 即退出码 1）=="
for f in $MINE; do
  [ -f "$f" ] || { echo "  [FAIL] 文件不存在：$f"; fail=1; continue; }
  hit=""
  for t in $BATCHB_SCOPED_TABLES; do
    code_lines=$(mktemp)
    grep -v -E '^[[:space:]]*(//|\*)' "$f" >"$code_lines" || true
    if grep -q "FROM $t" "$code_lines"; then
      if grep -q "SCOPE-EXEMPT" "$f" || grep -q "scope::" "$code_lines"; then
        hit="$hit $t(有豁免)"
      else
        hit="$hit $t(未豁免!)"
        fail=1
      fi
    fi
    rm -f "$code_lines"
  done
  printf '  %-58s %s\n' "${f#"$SRC"/}" "${hit:-无登记表查询}"
done

if [ "$fail" -eq 0 ]; then
  echo "==> 闸② PASS（本卡文件全部合规）"
else
  echo "==> 闸② FAIL（本卡有文件未写 SCOPE-EXEMPT）"
  exit 1
fi
