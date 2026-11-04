#!/usr/bin/env bash
# ==============================================================
# P2-6 证据探针：流程「模板库」到底有没有独立实体？
#
# 用途：只读。把「模板库 = biz_flow_def 的一个 flag」这个结论变成可复跑的断言，
#       而不是读代码读出来的印象。所有 SQL 都是 SELECT / count，不改任何数据。
#
# 用法：
#   bash scripts/verify/p2_6_template_identity_probe.sh            # 跑一轮（人读）
#   bash scripts/verify/p2_6_template_identity_probe.sh --json     # 跑一轮（机器读，用于两轮 diff）
#
# 库连接：优先 NIK_DATABASE_HOST/PORT/USER/NAME/PASSWORD；
#         缺省回落到 /root/.cloudpivot/snapshot-audit.env（本机隔离库，600）。
#
# 【P2-6d 落地后口径变更（2026-11-04 决策 A）】
#   `docs/architecture/P2-6-模板实体-决策.md` §3.3/§3.4 已拍板「合并概念」：菜单从 2 个
#   入口收敛为 1 个，`sys_menu` 里 `path='/flow/templates'` 的节点（V0194 种下，id 尾部
#   …07a5）由迁移 V0234 软删（`deleted_at` 置位，不硬删）。所以 P5 组里
#     · P5a「存在 /flow/templates 节点」            期望 1 → **0**
#     · P5c「两个入口并存、指向同一张表」          期望 1 → **0**
#   断言一条都没删（保留可追溯性），只翻期望值 / 收紧度量口径：
#     · P5b 改成查**含软删行**的节点 —— 它现在证的是「软删≠硬删，回滚有路」；
#     · P5d 是本次新增，单独钉住「唯一入口 /flow/def 仍在用」，防止收口把入口删光；
#     · P5c 改度量「两个在用入口是否并存」（=2 → 1，否则 0），才是它描述的那件事。
#
# 退出码：0 = 全部探针与预期一致；1 = 有探针不符（说明结论/现状变了，先看输出再改文档）。
# ==============================================================
set -uo pipefail

JSON=0
[[ "${1:-}" == "--json" ]] && JSON=1

ENV_FILE="${NIK_ENV_FILE:-/root/.cloudpivot/snapshot-audit.env}"
# 调用方显式给的变量优先于 env 文件（env 文件只是个缺省来源，不能反过来盖掉 CLI 上的指定）
_OVR_HOST="${NIK_DATABASE_HOST:-}"; _OVR_PORT="${NIK_DATABASE_PORT:-}"
_OVR_USER="${NIK_DATABASE_USER:-}"; _OVR_NAME="${NIK_DATABASE_NAME:-}"
_OVR_PASS="${NIK_DATABASE_PASSWORD:-}"
if [[ -r "$ENV_FILE" ]]; then
    # shellcheck disable=SC1090
    set -a; source "$ENV_FILE"; set +a
fi
[[ -n "$_OVR_HOST" ]] && NIK_DATABASE_HOST="$_OVR_HOST"
[[ -n "$_OVR_PORT" ]] && NIK_DATABASE_PORT="$_OVR_PORT"
[[ -n "$_OVR_USER" ]] && NIK_DATABASE_USER="$_OVR_USER"
[[ -n "$_OVR_NAME" ]] && NIK_DATABASE_NAME="$_OVR_NAME"
[[ -n "$_OVR_PASS" ]] && NIK_DATABASE_PASSWORD="$_OVR_PASS"

DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-alun}"
DB_NAME="${NIK_DATABASE_NAME:-cloudpivot_audit}"
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"

q() {  # q <sql> -> -tAc 单值
    psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -tAc "$1" 2>/dev/null
}

FAIL=0
declare -a ROWS=()

# probe <编号> <说明> <预期> <实际>
probe() {
    local id="$1" desc="$2" expect="$3" actual="$4"
    local mark="OK"
    if [[ "$expect" != "-" && "$actual" != "$expect" ]]; then
        mark="MISMATCH"; FAIL=1
    fi
    ROWS+=("$(printf '%-6s %-8s expect=%-10s actual=%-10s %s' "$id" "$mark" "$expect" "$actual" "$desc")")
}

# ---------- P0 连通性 ----------
if [[ -z "$(q 'select 1')" ]]; then
    echo "FATAL: 连不上库 $DB_USER@$DB_HOST:$DB_PORT/$DB_NAME（示例：NIK_DATABASE_PASSWORD=xx bash $0）" >&2
    exit 2
fi

# ---------- P1 定义总数 / 标准标记数 ----------
q "select count(*) from biz_flow_def where deleted_at is null" >/dev/null
DEF_TOTAL=$(q "select count(*) from biz_flow_def where deleted_at is null")
SYS_TRUE=$(q  "select count(*) from biz_flow_def where deleted_at is null and is_system")
SYS_FALSE=$(q "select count(*) from biz_flow_def where deleted_at is null and not is_system")
probe P1a "未软删的流程定义行数（= 模板库页显示的『共 N 个模板』）" - "$DEF_TOTAL"
probe P1b "其中 is_system = TRUE（页面上挂『标准』标签）" - "$SYS_TRUE"
probe P1c "其中 is_system = FALSE（页面上没有『标准』标签）" - "$SYS_FALSE"

# ---------- P2 关键反证：非标准行里有多少是「名字自报标准」的种子 ----------
# 这是本卡最硬的一条：种子迁移插 def 时没写 is_system → 默认 FALSE → 标准流程被错标成客户自建。
MISLABELED=$(q "select count(*) from biz_flow_def
                 where deleted_at is null and not is_system and flow_name like '%标准%'")
MISLABELED_LIST=$(q "select string_agg(flow_code || '(' || flow_name || ')', ' | ' order by flow_code)
                       from biz_flow_def
                      where deleted_at is null and not is_system and flow_name like '%标准%'")
# P2-6a 修完错标后，这里的不变量是「名字自称『标准』 ⇒ is_system 必须 TRUE」；
# 反过来不再成立（`budget_approve` / `asset_disposal` 等预置流程名字里就没有『标准』两个字，
# 它们 is_system=FALSE 属另一类问题，不由本探针判定）。
probe P2a "is_system=FALSE 却自称『标准』的行数（错标，必须 0）" - "$MISLABELED"
probe P2b "错标行数是否为 0（名字含『标准』的行都已标 is_system=TRUE）" 0 "$MISLABELED"

# ---------- P3 客户真正自建的流程有多少（另存为副本 / 自定义编码） ----------
CUSTOM=$(q "select count(*) from biz_flow_def
             where deleted_at is null and not is_system and flow_code like '%\_custom' escape '\'")
probe P3a "另存为出来的副本数（flow_code 以 _custom 结尾）" 0 "$CUSTOM"

# ---------- P4 标准流程接口与流程定义列表是不是同一张表 ----------
# P2-6b 之后：`model::templates(scope)` 在默认口径（`TemplateScope::Standard`）里带
# `AND d.is_system`；`TemplateScope::All` 才不过滤。静态核对「过滤在不在」。
SRC="$(dirname "$0")/../../backend/src/business/flow/model.rs"
FILTER=$(grep -c 'is_system' "$SRC" 2>/dev/null || echo 0)
WHERE_SYS=$(awk '/pub async fn templates\(/,/\}\);/' "$SRC" 2>/dev/null \
            | grep -c 'AND d\.is_system' || true)
probe P4a "model::templates() 标准口径带 is_system 过滤（应 = 1；P2-6b 前这里是 0）" 1 "$WHERE_SYS"

# ---------- P5 菜单：P2-6d 后入口从 2 个收敛为 1 个（口径变更理由见文件头注释） ----------
MENU_TPL=$(q "select count(*) from sys_menu where path = '/flow/templates' and deleted_at is null" 2>/dev/null)
MENU_TPL_TOP=$(q "select count(*) from sys_menu where path = '/flow/templates' and parent_id is null" 2>/dev/null)
MENU_DEF=$(q "select count(*) from sys_menu where path = '/flow/def' and deleted_at is null" 2>/dev/null)
# 「两个入口并存」= /flow/def 与 /flow/templates 同为**在用**（一张表两份入口）时记 1，否则 0。
# P2-6d 前 = 1（病态：同一张表两个入口，用户分不清该点哪个）；P2-6d 后 = 0。
MENU_TWO_ENTRIES=$(q "select case when count(*) = 2 then 1 else 0 end
                        from sys_menu
                       where path in ('/flow/def', '/flow/templates')
                         and deleted_at is null" 2>/dev/null)
probe P5a "存在 **在用**的 /flow/templates 菜单节点（P2-6d 已软删，期望 1→0）" 0 "$MENU_TPL"
probe P5b "/flow/templates 节点行仍在库内（软删非硬删）且仍是顶层节点（可回滚）" 1 "$MENU_TPL_TOP"
probe P5c "两个入口并存（/flow/def + /flow/templates 同时在用，一张表两份入口）—— P2-6d 后须 0" 0 "$MENU_TWO_ENTRIES"
probe P5d "/flow/def（审批流程定义）唯一入口仍在用（收口不许把入口删光）" 1 "$MENU_DEF"

# ---------- P6 版本 / 实例规模（说明 def 本身已是「可复用骨架」） ----------
VER=$(q "select count(*) from biz_flow_def_version")
INS=$(q "select count(*) from biz_flow_instance where deleted_at is null")
probe P6a "流程版本行总数（def → version → instance 三层）" - "$VER"
probe P6b "在跑/历史审批实例总数（def 已被复用发起的次数）" - "$INS"

if [[ $JSON -eq 1 ]]; then
    printf '{"db":"%s","def_total":%s,"is_system_true":%s,"is_system_false":%s,"mislabeled":%s,"custom":%s,"versions":%s,"instances":%s,"fail":%s}\n' \
        "$DB_NAME" "$DEF_TOTAL" "$SYS_TRUE" "$SYS_FALSE" "$MISLABELED" "$CUSTOM" "$VER" "$INS" "$FAIL"
else
    echo "== P2-6 探针 @ ${DB_NAME} $(date -Is) =="
    printf '%s\n' "${ROWS[@]}"
    echo "-- 错标明细：${MISLABELED_LIST:-（无）}"
    echo "-- 结论：FAIL=${FAIL}（0 = 现状与结论一致；1 = 现状已变，先看输出再改文档）"
fi
exit $FAIL
