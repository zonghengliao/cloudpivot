#!/usr/bin/env bash
# ============================================================================
# M4-22 采购缺口收口（计划 + 框架 + 评估 + 合同号）· 汇总演练
#
# 卡：t_562e8b43（M4-22【M4·采购】采购缺口收口：计划 + 框架 + 评估 + 合同号）
#
# 这一卡不写新功能，只做一件事：**把 22 张卡的成果拼起来，证明「缺口真的填上了」**。
# 所以本脚本验的不是某一张卡，而是四条主线拼合后的**一致性**：
#
#   1. 迁移面：M4 段 V0446…V0458 共 13 条，在模板克隆库上补齐；**再跑一遍必须零 ERROR**
#      （幂等；全新安装 / 灾备重建走的就是这条路）
#   2. 结构面（DB）：6 张表 + 6 个视图 + 采购单 4 个合同/协议列 + 合同对偶 CHECK + 索引
#      + 3 套新审批流（pp_approve / fw_agreement / supplier_access）PUBLISHED
#   3. 接口面（源码 ↔ 码表三向对拍，`m422_purchase_scope_audit.py`）：
#      40 条范围内路由全部挂权限码 · 声明 path 与路由宏一致 · 源码用到的 26 个码在
#      `sys_menu` 全有承载 · 25 个按钮码无摆设 · 4 个页面显形码后端零引用（约定 14）
#   4. 用例面（真库，--test-threads=1，每组 SKIP 必须为 0）：
#      计划 117 / 框架 101 / 评估 71 / 准入 20 / 合同执行 26 / E2E-20 3 / E2E-21 5
#      + 三组回归（采购域 459 / 合同域 109 / 收货域 165）
#   5. 卫生面：本批业务单据（`M4xx-` 单号）8 张表 0 残留 · 明细无孤儿 ·
#      4 类审批实例无在跑（模板自带的 PR_APPROVE 2 条按基线口径只报不闸）·
#      主数据（物料/供应商 `M4xx-`）走 `test_seed::ensure_*` 共享基线，**只报计数不设闸**
#   6. 门禁面：`permission_audit_tests`（8 条）+ `menu_invariants.sh`（8 条）
#   7. 有牙：`TEETH=1` 时坏连接串必须让 E2E 两组红（拒绝静默 SKIP 假绿）
#   8. 可选 `FRESH=1`：空库按版本号重放**全部** up 迁移（350+ 条），证明 M4 段不依赖
#      「更晚才种下的行」（乱序依赖）——这一档默认关，跑一次约 1-2 分钟
#
# 用法：
#   bash scripts/verify/m422_purchase_gap_closeout_drill.sh                    # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m422_purchase_gap_closeout_drill.sh  # 在 145 上跑
#   KEEP_DB=1     … 留库排查（打印库名）
#   TEETH=1       … 有牙自检（坏连接串必须红）
#   FRESH=1       … 加跑空库全链重放
#   SKIP_GATES=1  … 跳过 permission_audit / menu_invariants 两档（省 1 分钟）
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP；
# 连接串口令一律复用 `$PGPASSWORD`（URL 里写占位符会让真库用例连不上、打 SKIP 还记 ok，是假绿）。
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
BACKEND="$ROOT/backend"
AUDIT_PY="$ROOT/scripts/verify/m422_purchase_scope_audit.py"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="${M422_DB:-cp_m422_closeout_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
TEETH="${TEETH:-0}"
FRESH="${FRESH:-0}"
SKIP_GATES="${SKIP_GATES:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m422_drill_$$.log"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

# 库名护栏（任何写操作之前）：只许碰「演练库」形态的名字
case "$DB" in
    *drill*|*e2e*|*test*) :;;
    *) printf '库名 %s 不含 drill/e2e/test，拒绝在它上面跑（rc=2）\n' "$DB"; exit 2;;
esac

# 自己建的库才由自己删；`M422_DB` 是调用方给的库 → 只跑不清
OWN_DB=1
if [ -n "${M422_DB:-}" ]; then OWN_DB=0; fi

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }

assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}
assert_ge() { # 下限 实际 说明
    if [ "${2:-0}" -ge "$1" ] 2>/dev/null; then ok "$3（=$2）"; else bad "$3（期望 ≥ $1，实际 [$2]）"; fi
}

cleanup() {
    if [ "$OWN_DB" = "0" ] || [ "$KEEP_DB" = "1" ]; then
        printf '\n  （库保留 → %s；不删）\n' "$DB"
        return
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    rm -f "$LOG"
}
trap cleanup EXIT

printf '== M4-22 采购缺口收口演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

# ---------- 0) 模板库 ----------
echo "== 0) 前置：模板库可克隆"
if ! psql_admin "SELECT 1 FROM pg_database WHERE datname = '$TEMPLATE_DB'" | grep -q 1; then
    bad "模板库 $TEMPLATE_DB 不存在（请先在两台机器的 PG 里准备模板库）"
    exit 2
fi
ok "模板库 $TEMPLATE_DB 存在"

if [ "$OWN_DB" = "1" ]; then
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    if psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE_DB" >/dev/null 2>&1; then
        ok "从模板克隆演练库 $DB"
    else
        bad "克隆演练库失败"
        exit 2
    fi
else
    ok "使用调用方给的演练库 $DB（只跑不清）"
fi

# ---------- 1) 补齐业务迁移 + 幂等复跑 ----------
echo "== 1) 迁移面：补齐 V0405…最新，并把 M4 段 V0446…V0458 再跑一遍（幂等）"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR" | grep -E '^V[0-9]{4}__.*\.up\.sql$' | sort); do
    ver="${f:1:4}"
    if [ "$((10#$ver))" -le 404 ]; then continue; fi
    APPLIED=$((APPLIED+1))
    if ! psql_file "$MIG_DIR/$f"; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$f（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
if [ "$MIG_FAIL" -eq 0 ]; then ok "$APPLIED 条业务迁移全部应用成功"; fi

M4_FILES=0
M4_REAPPLY_FAIL=0
for f in $(ls "$MIG_DIR" | grep -E '^V04(4[6-9]|5[0-8])__.*\.up\.sql$' | sort); do
    M4_FILES=$((M4_FILES+1))
    if ! psql_file "$MIG_DIR/$f"; then
        M4_REAPPLY_FAIL=$((M4_REAPPLY_FAIL+1))
        bad "M4 段迁移第二遍报错：$f（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
assert_eq "13" "$M4_FILES" "M4 段迁移文件 13 条（V0446…V0458，跨目录无重号）"
assert_eq "0" "$M4_REAPPLY_FAIL" "M4 段 13 条迁移**第二遍零 ERROR**（幂等；空库重建/灾备走同一条路）"

# ---------- 2) 结构面（DB） ----------
echo "== 2) 结构面：6 表 + 6 视图 + 采购单 4 列 + 合同对偶 CHECK + 3 套审批流"
assert_eq "biz_purchase_plan"          "$(psql_q "SELECT to_regclass('public.biz_purchase_plan')")"          "主题① 采购计划头表（V0446）"
assert_eq "biz_purchase_plan_item"     "$(psql_q "SELECT to_regclass('public.biz_purchase_plan_item')")"    "主题① 采购计划明细（V0446）"
assert_eq "biz_framework_agreement"    "$(psql_q "SELECT to_regclass('public.biz_framework_agreement')")"   "主题② 框架协议头表（V0450）"
assert_eq "biz_framework_agreement_item" "$(psql_q "SELECT to_regclass('public.biz_framework_agreement_item')")" "主题② 框架协议明细（V0450）"
assert_eq "biz_supplier_evaluation"    "$(psql_q "SELECT to_regclass('public.biz_supplier_evaluation')")"   "主题③ 供应商评估（V0454）"
assert_eq "biz_supplier_access"        "$(psql_q "SELECT to_regclass('public.biz_supplier_access')")"       "主题③ 供应商准入（V0455）"
for v in v_purchase_plan_execution v_purchase_plan_item_execution v_framework_agreement_execution v_framework_agreement_item_execution v_contract_execution v_contract_execution_order; do
    assert_eq "$v" "$(psql_q "SELECT to_regclass('public.$v')")" "视图 $v 存在"
done
assert_eq "4" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_name='biz_purchase_order' AND column_name IN ('contract_id','contract_no_snapshot','framework_agreement_id','framework_agreement_no_snapshot')")" "主题④ 采购单 4 个合同/协议列都在（contract_id 由 V0198 早已落地）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM pg_constraint WHERE conrelid='biz_purchase_order'::regclass AND conname='ck_biz_po_contract_pair'")" "合同对偶 CHECK ck_biz_po_contract_pair 在位（CONTRACT⇔有合同号 / SPOT⇔无）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM pg_indexes WHERE tablename='biz_purchase_order' AND indexname='idx_biz_po_contract'")" "合同索引 idx_biz_po_contract 在位"
assert_eq "3" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE deleted_at IS NULL AND status='PUBLISHED' AND flow_code IN ('pp_approve','fw_agreement','supplier_access')")" "3 套新审批流已发布（pp_approve / fw_agreement / supplier_access）"
assert_eq "24" "$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND (auth_code LIKE 'business:erp:purchase-plan:%' OR auth_code LIKE 'business:erp:framework-agreement:%' OR auth_code LIKE 'business:erp:supplier-evaluation:%' OR auth_code LIKE 'business:erp:supplier-access:%' OR auth_code='business:erp:contract:execution')")" "四主题菜单 24 行（4 页面 + 20 按钮）"
assert_eq "4" "$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND menu_type=2 AND path IN ('/business/erp/purchase-plan','/business/erp/framework-agreement','/business/erp/supplier-access','/business/erp/supplier-evaluation')")" "4 个页面行 path 挂在 /business/erp 下（侧栏显形）"
assert_ge 24 "$(psql_q "SELECT count(*) FROM sys_role r JOIN sys_role_menu rm ON rm.role_id=r.id JOIN sys_menu m ON m.id=rm.menu_id WHERE r.role_code='PURCHASE_MANAGER' AND r.deleted_at IS NULL AND (m.auth_code LIKE 'business:erp:purchase-plan:%' OR m.auth_code LIKE 'business:erp:framework-agreement:%' OR m.auth_code LIKE 'business:erp:supplier-evaluation:%' OR m.auth_code LIKE 'business:erp:supplier-access:%' OR m.auth_code='business:erp:contract:execution')")" "PURCHASE_MANAGER 对四主题全量授权（≥24 码）"

# 前端面：4 个页面 + 4 个 api 模块 + 路由登记
echo "== 2b) 前端面：4 个页面 / 4 个 api / 4 条路由登记"
FE_OK=0
for mod in purchase-plan framework-agreement supplier-access supplier-evaluation; do
    if [ -f "$ROOT/frontend/src/views/business/erp/$mod/index.vue" ] && [ -f "$ROOT/frontend/src/api/business/erp/$mod.ts" ] \
       && grep -q "business/erp/$mod" "$ROOT/frontend/src/router/index.ts"; then
        FE_OK=$((FE_OK+1))
    else
        bad "前端缺件：$mod（页面 / api / 路由登记 三处必须齐）"
    fi
done
assert_eq "4" "$FE_OK" "四个前端页面「页面 + api + 路由」三件齐"

# ---------- 3) 接口面三向对拍 ----------
echo "== 3) 接口面：源码路由 ↔ 权限码 ↔ sys_menu（三向对拍）"
MENU_TSV="$TMP/m422_menu_$$.tsv"
psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tA -F $'\t' \
    -c "SELECT coalesce(auth_code,'-'), menu_type::text, coalesce(path,'-'), title FROM sys_menu WHERE deleted_at IS NULL AND menu_type IN (2,3) AND (auth_code LIKE 'business:erp:purchase-plan:%' OR auth_code LIKE 'business:erp:framework-agreement:%' OR auth_code LIKE 'business:erp:supplier-evaluation:%' OR auth_code LIKE 'business:erp:supplier-access:%' OR auth_code LIKE 'business:erp:purchase-order:%' OR auth_code='business:erp:contract:execution') ORDER BY 1" >"$MENU_TSV"
AUDIT_OUT="$TMP/m422_audit_$$.out"
( cd "$ROOT" && python3 "$AUDIT_PY" --menu-tsv "$MENU_TSV" ) >"$AUDIT_OUT" 2>&1
AUDIT_RC=$?
sed -n 's/^  PASS /  PASS /p; s/^  FAIL /  FAIL /p' "$AUDIT_OUT" | while IFS= read -r line; do printf '%s\n' "$line"; done
AUDIT_PASS="$(grep -c '^  PASS' "$AUDIT_OUT" || true)"
AUDIT_FAIL="$(grep -c '^  FAIL' "$AUDIT_OUT" || true)"
PASS=$((PASS+AUDIT_PASS)); FAIL=$((FAIL+AUDIT_FAIL))
if [ "$AUDIT_RC" -eq 0 ]; then
    ok "三向对拍审计全绿（$AUDIT_PASS 条断言，见 $AUDIT_OUT）"
else
    bad "三向对拍审计有 $AUDIT_FAIL 条不通过（见 $AUDIT_OUT）"
    grep -E '^  FAIL' "$AUDIT_OUT" | head -10 | sed 's/^/        /'
fi

# ---------- 4) 有牙自检 ----------
if [ "$TEETH" = "1" ]; then
    echo "== 4) 有牙自检：坏连接串必须让两组 E2E 红（否则「全绿」是假绿）"
    teeth_out="$TMP/m422_teeth_$$.out"
    ( cd "$BACKEND" && ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/cp_m422_no_such_db" \
        cargo test -p cloudpivot --lib business::erp::purchase::purchase_chain_e2e_tests -- --test-threads=1 ) >>"$teeth_out" 2>&1
    teeth_rc1=$?
    ( cd "$BACKEND" && ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/cp_m422_no_such_db" \
        cargo test -p cloudpivot --lib business::erp::purchase::framework_contract_e2e_tests -- --test-threads=1 ) >>"$teeth_out" 2>&1
    teeth_rc2=$?
    teeth_line="$(grep -E '^test result: ' "$teeth_out" | tr '\n' ' ')"
    info "坏连接串：rc=$teeth_rc1/$teeth_rc2 · $teeth_line"
    if [ "$teeth_rc1" -ne 0 ] && [ "$teeth_rc2" -ne 0 ] \
       && printf '%s' "$teeth_line" | grep -q '3 failed' && printf '%s' "$teeth_line" | grep -q '5 failed'; then
        ok "有牙：坏连接串下 E2E 两组分别 3 / 5 条全失败（拒绝静默 SKIP 假绿）"
    else
        bad "失牙：坏连接串下没有全失败（假绿风险）"
    fi
    printf '\n== 汇总（有牙模式）：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
    if [ "$FAIL" -eq 0 ]; then echo "ALL PASS"; exit 0; fi
    exit 1
fi

# ---------- 5) 演练库清污（只针对模板库自带的一条历史残留） ----------
# 模板库 `cloudpivot_test` 自带一条 2026-11-10 的 `PO_APPROVE | PENDING`（订单 `W1B-PO-0001`），
# 采购域收尾用例的断言口径是「全库不剩在跑的采购审批实例」，会被这条与本批无关的残留顶红。
# 只按**单号白名单**删它，并打印影响行数；别的单号一行不碰。
echo "== 5) 演练库清污：模板自带的 W1B-PO-0001 历史 PENDING 审批实例"
STALE_TASKS="$(psql_q "WITH stale AS (SELECT i.id FROM biz_flow_instance i JOIN biz_purchase_order po ON po.id = i.biz_id WHERE i.biz_type = 'PO_APPROVE' AND i.status = 'PENDING' AND i.deleted_at IS NULL AND po.order_no = 'W1B-PO-0001') DELETE FROM biz_flow_task WHERE instance_id IN (SELECT id FROM stale)")"
STALE_NODES="$(psql_q "WITH stale AS (SELECT i.id FROM biz_flow_instance i JOIN biz_purchase_order po ON po.id = i.biz_id WHERE i.biz_type = 'PO_APPROVE' AND i.status = 'PENDING' AND i.deleted_at IS NULL AND po.order_no = 'W1B-PO-0001') DELETE FROM biz_flow_instance_node WHERE instance_id IN (SELECT id FROM stale)")"
STALE_INST="$(psql_q "DELETE FROM biz_flow_instance i USING biz_purchase_order po WHERE po.id = i.biz_id AND i.biz_type = 'PO_APPROVE' AND i.status = 'PENDING' AND i.deleted_at IS NULL AND po.order_no = 'W1B-PO-0001'")"
info "清污影响行数：待办 [$STALE_TASKS] / 节点 [$STALE_NODES] / 实例 [$STALE_INST]"

# ---------- 6) 真库用例（按主题分组） ----------
echo "== 6) 真库用例（--test-threads=1；每组 SKIP 必须为 0）"
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"

run_group() { # 模块路径 最少用例数 说明
    local module="$1" min="$2" label="$3" out line passed failed skipped
    out="$TMP/m422_$$_${module//[:/]/_}.out"
    ( cd "$BACKEND" && cargo test -p cloudpivot --lib "$module" -- --test-threads=1 ) >"$out" 2>&1
    line="$(grep -E '^test result: ' "$out" | tail -1)"
    passed="$(printf '%s' "$line" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
    failed="$(printf '%s' "$line" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
    skipped="$(grep -c '^SKIP' "$out" || true)"
    [ -z "${passed:-}" ] && passed=0
    [ -z "${failed:-}" ] && failed=0
    [ -z "${skipped:-}" ] && skipped=0
    if [ "${failed:-0}" -eq 0 ] && [ "${passed:-0}" -ge "$min" ]; then
        ok "$label：$module 全绿（$passed 条 ≥ 基线 $min）"
    elif [ "${failed:-0}" -ne 0 ]; then
        bad "$label：$module 有 $failed 条失败（见 $out）"
        grep -E 'panicked at|assertion|left:|right:' -A 2 "$out" | head -30
        printf '  ---- 步骤轨迹（尾 10 行）：\n'
        grep -E '^\[' "$out" | tail -10 | sed 's/^/        /'
    else
        bad "$label：$module 只跑了 $passed 条（基线 ≥ $min，见 $out）"
    fi
    if [ "$skipped" -eq 0 ]; then
        ok "$label：SKIP=0（真库用例没被「无 ERP_DB_URL」打成假绿）"
    else
        bad "$label：出现 $skipped 行 SKIP（真库用例被跳过 = 假绿）"
    fi
}

# 主题① 采购计划（M4-01…06：结构 15 + CRUD/审批 + 转申请 + 执行率 + 单测）
run_group "business::erp::purchase::purchase_plan" 117 "主题① 计划（结构/CRUD/审批/转申请/执行率/单测）"
# 主题② 框架协议（M4-07…11b：结构 + 订单引用 + 执行量 + 超限反例 + 维护链路）
run_group "business::erp::purchase::framework_agreement" 101 "主题② 框架协议（引用/执行量/超限/维护）"
# 主题③ 供应商评估（M4-12…16：四维打分 + 评级 + 台账排名 + 交期参考分）
run_group "business::erp::purchase::supplier_evaluation" 71 "主题③ 供应商评估（打分/评级/台账/排名）"
# 主题③ 供应商准入 / 黑名单（M4-14）
run_group "business::erp::purchase::supplier_access" 20 "主题③ 供应商准入（申请单 + 启停闸）"
# 主题④ 合同执行量（M4-19）
run_group "business::erp::contract::contract_execution" 26 "主题④ 合同执行量（视图 + 两接口）"
# E2E：计划 → 申请 → 订单 → 入库（M4-20）
run_group "business::erp::purchase::purchase_chain_e2e_tests" 3 "E2E-20 计划→申请→订单→入库"
# E2E：框架协议 + 合同关联（M4-21）
run_group "business::erp::purchase::framework_contract_e2e_tests" 5 "E2E-21 框架协议 + 合同关联"
# 回归三组
run_group "business::erp::purchase" 459 "回归：采购域整组（四主题同域，互不踩脚）"
run_group "business::erp::contract" 109 "回归：合同域整组"
run_group "business::erp::stockin" 165 "回归：收货域整组（两条 E2E 都要过账到库存）"

# ---------- 7) 卫生面：夹具残留 ----------
echo "== 7) 卫生面：本批业务单据 0 残留 / 明细无孤儿 / 审批实例无在跑"
RES_PAT="^M4[0-9]{2}[A-Z]?[-_]"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_plan WHERE plan_no ~ '$RES_PAT'")" "biz_purchase_plan 无 M4xx 计划"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_framework_agreement WHERE agreement_no ~ '$RES_PAT'")" "biz_framework_agreement 无 M4xx 协议"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_supplier_evaluation WHERE evaluation_no ~ '$RES_PAT'")" "biz_supplier_evaluation 无 M4xx 评估"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_supplier_access WHERE access_no ~ '$RES_PAT'")" "biz_supplier_access 无 M4xx 准入单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_order WHERE order_no ~ '$RES_PAT'")" "biz_purchase_order 无 M4xx 采购单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_requisition WHERE req_no ~ '$RES_PAT'")" "biz_purchase_requisition 无 M4xx 申请单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_stock_in WHERE in_no ~ '$RES_PAT'")" "biz_stock_in 无 M4xx 收货单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_contract WHERE contract_no ~ '$RES_PAT'")" "biz_contract 无 M4xx 合同"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_plan_item i WHERE NOT EXISTS (SELECT 1 FROM biz_purchase_plan p WHERE p.id = i.plan_id)")" "计划明细无孤儿"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_framework_agreement_item i WHERE NOT EXISTS (SELECT 1 FROM biz_framework_agreement a WHERE a.id = i.agreement_id)")" "协议明细无孤儿"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE deleted_at IS NULL AND status='PENDING' AND biz_type IN ('PP_APPROVE','FWA_APPROVE','SUPPLIER_ACCESS')")" "本批三类审批无在跑实例（PP/FWA/SUPPLIER_ACCESS 模板里本来 0 条 → 全量闸）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE deleted_at IS NULL AND status='PENDING' AND biz_type='PO_APPROVE'")" "采购单审批无在跑实例（模板 W1B 残留已按单号清）"
assert_eq "2" "$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE deleted_at IS NULL AND status='PENDING' AND biz_type='PR_APPROVE'")" "申请单审批 PENDING = 2（模板库 2026-11-09 种子基线，与本批无关，只按基线比）"

# 主数据：`test_seed::ensure_*` 是**共享基线**（不清场是设计），只报计数，不设闸
MAT="$(psql_q "SELECT count(*) FROM biz_material WHERE material_code ~ '$RES_PAT'")"
SUP="$(psql_q "SELECT count(*) FROM biz_supplier WHERE supplier_code ~ '$RES_PAT'")"
info "主数据基线（不设闸）：物料 M4xx=$MAT / 供应商 M4xx=$SUP —— 一律走 test_seed::ensure_*，重复跑不增行"

# ---------- 8) 门禁面 ----------
if [ "$SKIP_GATES" = "1" ]; then
    info "SKIP_GATES=1：跳过 permission_audit / menu_invariants"
else
    echo "== 8) 门禁面：permission_audit_tests（8 条）+ menu_invariants.sh（8 条）"
    GA="$TMP/m422_perm_audit_$$.out"
    ( cd "$BACKEND" && cargo test -p cloudpivot --lib system::menu::permission_audit_tests -- --test-threads=1 ) >"$GA" 2>&1
    ga_line="$(grep -E '^test result: ' "$GA" | tail -1)"
    ga_pass="$(printf '%s' "$ga_line" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
    ga_fail="$(printf '%s' "$ga_line" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
    if [ "${ga_fail:-1}" -eq 0 ] && [ "${ga_pass:-0}" -ge 8 ]; then
        ok "permission_audit_tests 全绿（$ga_pass 条 ≥ 8）"
    else
        bad "permission_audit_tests 不达标（$ga_line，见 $GA）"
    fi
    MI="$TMP/m422_menu_inv_$$.out"
    PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" >"$MI" 2>&1
    mi_rc=$?
    if [ "$mi_rc" -eq 0 ]; then
        ok "menu_invariants.sh 8 条全绿（见 $MI）"
    else
        bad "menu_invariants.sh 报红 rc=$mi_rc（见 $MI：$(grep -E '✗|FAIL' "$MI" | head -3 | tr '\n' ' ')）"
    fi
fi

# ---------- 9) 可选：空库全链重放 ----------
if [ "$FRESH" = "1" ]; then
    echo "== 9) 空库全链重放：按版本号升序 apply 全部 up 迁移（证明无乱序依赖）"
    FDB="cp_m422_fresh_test_$$"
    case "$FDB" in *drill*|*e2e*|*test*) :;; *) bad "重放库名不合规"; exit 1;; esac
    psql_admin "DROP DATABASE IF EXISTS $FDB" >/dev/null 2>&1
    if psql_admin "CREATE DATABASE $FDB" >/dev/null 2>&1; then
        ok "空库已建 $FDB"
        FRESH_OK=0
        FRESH_BAD=0
        FIRST_BAD=""
        while IFS= read -r f; do
            if psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$FDB" -q -X -v ON_ERROR_STOP=1 \
                    --single-transaction -f "$f" >"$LOG" 2>&1; then
                FRESH_OK=$((FRESH_OK+1))
            else
                FRESH_BAD=$((FRESH_BAD+1))
                FIRST_BAD="$f"
                break
            fi
        done < <(find "$ROOT/backend/app/migrations" -name 'V*__*.up.sql' \
                 | awk -F'/V' '{split($NF, a, "__"); print a[1] + 0 "\t" $0}' | sort -n | cut -f2-)
        if [ "$FRESH_BAD" -eq 0 ]; then
            ok "空库重放 $FRESH_OK 条 up 迁移全过（含 M4 段 13 条）"
            assert_ge 180 "$(psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$FDB" -tAc "SELECT count(*) FROM information_schema.tables WHERE table_schema='public'")" "重放后空库表数 ≥ 180"
        else
            bad "空库重放失败：$FIRST_BAD（$(grep -m1 -E 'ERROR|FATAL' "$LOG")）"
        fi
        psql_admin "DROP DATABASE IF EXISTS $FDB" >/dev/null 2>&1
    else
        bad "空库建库失败：$FDB"
    fi
fi

# ---------- 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项，见上面的 FAIL 行"
exit 1
