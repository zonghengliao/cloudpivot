#!/usr/bin/env bash
# 提交前 / push 前 / 发版前的统一门禁。**分四层**（卡 t_2746ca53，ARCH-9 §4 口径）：
#
#   L0  静态闸（秒级，无编译、无库）  ：C7 G1–G6 / 色板 / 迁移文件名与版本号全局唯一 / 库存写入口（含自检）/ 迁移授权段角色码
#   L1  编译与类型闸（≈2 分钟）       ：cargo fmt --check / clippy --all-targets -D warnings
#                                       / cargo check / pnpm typecheck
#   L2  连库与存量档（分钟级，需库）   ：闸⓪真库用例（CHECK_WITH_DB=1）/ 批次B 闸①闸②
#                                       / 闸③ERP不变量 / 闸④词表 / 闸⑤快照 / 闸⑥死权限码 / 闸⑦孤儿按钮
#   L3  干净检出闸（≈15 秒）          ：`git archive HEAD` 提取干净树 → 跑 lib 单测
#                                       （证明「不依赖本机 gitignored 私有配置」，见 scripts/verify/clean_checkout_gate.sh）
#
# 用法：
#   bash scripts/check.sh --fast       # L0+L1（≈2–3 分钟）—— push 前的机械门禁，pre-push 钩子就调这个
#   bash scripts/check.sh --full       # L0+L1+L2+L3（不传参数时的默认行为，发版前必跑）
#   bash scripts/check.sh --dry-run    # 只打印会跑什么（含每项属于哪一层），不执行
#   bash scripts/check.sh --help       # 同上说明
#
#   叠加用法：bash scripts/check.sh --fast --dry-run   # 看 --fast 会跑什么
#
# 环境变量（与旧版完全一致，未改动语义）：
#   BATCHB_GATE=warn      批次B 两条门禁只告警不失败（B2–B4 施工期用）
#   CHECK_WITH_DB=1       开闸⓪真库用例（需 ERP_DB_URL / SYS_DB_URL / TASK_DB_URL 三个都指同一隔离库）
#   CHECK_WITH_DB_FILTER= 只跑某段真库用例
#   ERP_INVARIANTS_GATE / BIZ_VOCAB_GATE / SNAPSHOT_AUDIT_GATE / PERM_MENU_GATE / ORPHAN_BTN_GATE / ROLE_CODE_GATE
#                         = block（默认）/ warn / off
#
# 说明：本脚本的退出码 = 「本次所跑层里任一检查失败」。批次B 闸② 是**故意**会在当前树上报错的
#       —— 它度量的是「还没接入数据权限的存量查询」，见 docs/批次B-DDL.md §6 与
#       docs/后端二开指南.md「数据权限检查清单」。因此它属 L2（日常档），**不进 pre-push**。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ======================================================================
# 分层与参数
# ======================================================================
LAYERS="L0 L1 L2 L3"     # --full：全跑
DRY_RUN=0

usage() {
    # 打印文件头的注释块（第一个非注释行之前）
    awk 'NR > 1 { if ($0 ~ /^#/) { sub(/^# ?/, ""); print; next } else { exit } }' "${BASH_SOURCE[0]}"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --fast)    LAYERS="L0 L1"; shift ;;
        --full)    LAYERS="L0 L1 L2 L3"; shift ;;
        --l0)      LAYERS="L0"; shift ;;
        --l1)      LAYERS="L1"; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "未知参数：$1（支持 --fast / --full / --dry-run / --help）" >&2; exit 2 ;;
    esac
done

# want <层名>：本层是否启用
want() {
    local l="$1"
    case " $LAYERS " in *" $l "*) return 0 ;; *) return 1 ;; esac
}

LAYER_DESC="L0 静态闸（秒级）→ L1 编译与类型闸（≈2 分钟）"
if want L2; then LAYER_DESC="$LAYER_DESC → L2 连库与存量档"; fi
if want L3; then LAYER_DESC="$LAYER_DESC → L3 干净检出闸"; fi
echo "==> check.sh：本次执行 $LAYERS（$LAYER_DESC）"

# dry-run 用：声明一个计划项（层 · 说明 · 命令行）
plan() { printf '   [%s] %-44s %s\n' "$1" "$2" "${3:-}"; }

# 计划表：--dry-run 时只打这张表就退出。
# ⚠ 加/删门禁时要同步改这里 —— 它与脚本里实际执行的段落一一对应（顺序也一致）。
# 执行顺序就是脚本内顺序：L1 编译类（最快暴露语法/类型问题）→ 闸⓪/批次B（L2）
#   → L0 静态闸 → 闸③–⑦（L2）→ L3。层只决定「跑不跑」，不改变文件内顺序。
plan_all() {
    echo "==> 计划（--dry-run）："
    if want L1; then
        plan L1 "后端格式" "cd backend && cargo fmt -- --check"
        plan L1 "后端 lint（lib+bin+tests+examples，告警即错）" "cd backend && cargo clippy --all-targets -- -D warnings"
        plan L1 "后端类型检查" "cd backend && cargo check"
        if [ -d "$ROOT/frontend/node_modules" ]; then
            plan L1 "前端类型检查" "cd frontend && pnpm typecheck"
        else
            plan L1 "前端类型检查（缺 node_modules → 会跳过）" "cd frontend && pnpm typecheck"
        fi
    fi
    if want L2; then
        plan L2 "闸⓪ 真库用例（仅 CHECK_WITH_DB=1 时真跑）" "cd backend && cargo test --lib --features test-hooks -- --test-threads=1"
        plan L2 "批次B 闸① 分页唯一写法（paginate_with_params 必须 0 处）" "grep 静态检查（scripts/check.sh · batchb_gates）"
        plan L2 "批次B 闸② 登记表必须接 scope:: 或 SCOPE-EXEMPT" "python3 scripts/lint/scope_gate.py --root . --baseline 21"
    fi
    if want L0; then
        plan L0 "C7 变更日志回滚零残留 G1–G6" "grep/awk 静态检查（scripts/check.sh · batchc_gates）"
        plan L0 "偏好字典 ui.palette ↔ 前端色板一致" "python3 scripts/lint/palette_def_sync.py --root ."
        plan L0 "迁移版本号跨目录全局唯一（自检 + 扫描）" "python3 scripts/lint/migration_version_unique.py --selftest && python3 scripts/lint/migration_version_unique.py --root ."
        plan L0 "库存三本账唯一写入口（判据自检 + 扫描）" "python3 scripts/verify/inventory_write_gate.py [--self-test]"
        plan L0 "迁移授权段角色码（自检 + 扫描）" "python3 scripts/verify/role_code_gate.py [--self-test]"
    fi
    if want L2; then
        plan L2 "闸③ ERP 账务不变量" "python3 scripts/verify/erp_invariants.py"
        plan L2 "闸④ 业务类型词表值域" "python3 scripts/verify/biz_type_domain.py --strict"
        plan L2 "闸⑤ 快照一致性巡检" "python3 scripts/verify/snapshot_audit.py"
        plan L2 "闸⑥ 死权限码（handler 注解码必须有菜单承载）" "python3 scripts/verify/perm_menu_gate.py"
        plan L2 "闸⑦ 孤儿按钮" "python3 scripts/verify/orphan_button_gate.py"
    fi
    if want L3; then
        plan L3 "干净检出闸（git archive HEAD → lib 单测）" "bash scripts/verify/clean_checkout_gate.sh"
    fi
    echo ""
    echo "   （L2 里 闸③–⑦ 需要库口令；无库环境按「无 DB 环境」跳过，与旧版口径一致）"
}

if [ "$DRY_RUN" = "1" ]; then
    plan_all
    exit 0
fi

# ======================================================================
# L1 编译与类型闸
# ======================================================================
run_l1() {
    echo "==> L1 后端：cargo fmt -- --check"
    (cd "$ROOT/backend" && cargo fmt -- --check)

    echo "==> L1 后端：cargo clippy --all-targets -- -D warnings"
    (cd "$ROOT/backend" && cargo clippy --all-targets -- -D warnings)

    echo "==> L1 后端：cargo check"
    (cd "$ROOT/backend" && cargo check)

    echo "==> L1 前端：pnpm typecheck"
    # 干净克隆里没有 node_modules（.gitignore）；这时跳过而不是判红，
    # 否则「干净检出必红」，与本仓 2026-11-10 审查口径冲突（L3 闸同一条原理）。
    if [ ! -d "$ROOT/frontend/node_modules" ]; then
        echo "  ℹ frontend/node_modules 不存在 → 跳过（先 cd frontend && pnpm install；CI 会跑）"
    elif ! command -v pnpm >/dev/null 2>&1; then
        echo "  ℹ 本机没有 pnpm → 跳过前端类型检查（CI 会跑）"
    else
        (cd "$ROOT/frontend" && pnpm typecheck)
    fi
}

if want L1; then
    run_l1
fi


# ======================================================================
# 闸⓪ · 真库用例（**可选档**：`CHECK_WITH_DB=1`，卡 t_977c5189）
#
# # 为什么需要这一档
#
# 上面几条 `cargo` 门禁都**不连库**。真库用例的写法是
# `let Ok(url) = env::var("ERP_DB_URL") else { println!("SKIP: …"); return; }`，
# 而 libtest 把「打印 SKIP 后 return」记成 **passed** ⇒ 默认门禁跑出
# 「2910 passed / 0 failed」全绿，可是**一条真库断言都没执行**。
#
# 代价是实测过的：`payable_bridge`（P17）夹具只在「下一个用例的 setup」里清场，
# 模块跑完库里留下 2 个**在职超管**（`p17_ap1/p17_ap2`），而 `FALLBACK_ADMIN` 的会签
# 人数 = 在职超管数 ⇒ `flow::edge_admin_tests` 的 3 条批量审批用例集体假红
# （落边 0 行 / 实例停在 PENDING）。默认门禁全程绿，只有「真库上跑全量」才暴露。
#
# # 用法
#
#   CHECK_WITH_DB=1 bash scripts/check.sh                  # 全量 lib（约 12 分钟）
#   CHECK_WITH_DB=1 CHECK_WITH_DB_FILTER=business::flow \\
#       bash scripts/check.sh                              # 只跑某段（提交前更快）
#   ERP_DB_URL=… SYS_DB_URL=… TASK_DB_URL=… CHECK_WITH_DB=1 bash scripts/check.sh
#
# 没有现成库时先建一个干净隔离库（发布二进制首启会自动套完迁移）：
#   bash scripts/verify/erp_clean_db.sh
#
# # 判据（三条都硬）
#
#   ① 任何一条用例失败 → 红；
#   ② 出现「**库没配 / 连不上**」类 SKIP（`未设置 XXX_DB_URL` / `XXX_DB_URL 连接失败` /
#      `set_db 失败`）→ 红（libtest 把这类 return 记成 passed，就是假绿）；
#      「数据前提缺失」类 SKIP（`本库的 5602 仍是末级科目` 等）只打 ℹ 不判红 —— 那是如实
#      报告环境差异，判红会把干净库上的正常情况变成假缺陷；
#   ③ 三个库 URL 缺一个 → 红（只给 ERP_DB_URL 会让 `flow::model` / task 模块静默 SKIP）。
#
# 不传 `CHECK_WITH_DB_FILTER`（= 全量）时自动追加
# `--skip business::flow::internal_control::db_tests`：那组用例按设计要求独占库
# （`P0FIX_DB_URL`），同库全量时必须排除（与 `scripts/verify/run_lib_tests_by_module.sh` 同口径）。
# ======================================================================

# ---- L2 层起点 --------------------------------------------------------
if ! want L2; then
    echo "==> 闸⓪：真库用例 —— 跳过（本档不含 L2；--fast 只跑 L0+L1）"
elif [ "${CHECK_WITH_DB:-0}" = "1" ]; then
    echo "==> 闸⓪：真库用例（cargo test --lib --features test-hooks -- --test-threads=1）"
    missing=""
    for var in ERP_DB_URL SYS_DB_URL TASK_DB_URL; do
        if [ -z "${!var:-}" ]; then missing="$missing $var"; fi
    done
    if [ -n "$missing" ]; then
        echo "  ✗ CHECK_WITH_DB=1 但库 URL 不全：缺$missing" >&2
        echo "    只给 ERP_DB_URL 会让 flow::model / task_bridge / notify_node 的真库用例静默 SKIP（假绿）。" >&2
        echo "    修法：三个都指同一个隔离库，或先 bash scripts/verify/erp_clean_db.sh 建一个。" >&2
        exit 1
    fi
    db_log="$(mktemp /tmp/check_with_db.XXXXXX.log)"
    db_rc=0
    # 参数口径：
    #   · `--test-threads=1`：lib 里有改全局配置（流程定义 / 状态机边 / 期末结账）的用例，
    #     并行会互踩（详见 skill/SKILL.md「测试卫生」与 docs/测试卫生-真库连库入口清单.md）；
    #   · 不传过滤（全量）时排除 `business::flow::internal_control::db_tests`：那组用例按设计
    #     要求**独占库**（`P0FIX_DB_URL`），同库全量时必须 `--skip`，否则它会主动 panic 指路。
    #     （skill §7 / scripts/verify/run_lib_tests_by_module.sh 同一口径）
    #   · `--nocapture`：真库用例的 SKIP 是 `println!` 出来的，libtest 默认**只在失败时**
    #     才把 stdout 打出来 ⇒ 不加 `--nocapture` 就永远 grep 不到 SKIP（等于没有这道判据）。
    # shellcheck disable=SC2086
    db_args="--test-threads=1 --nocapture ${CHECK_WITH_DB_FILTER:-}"
    if [ -z "${CHECK_WITH_DB_FILTER:-}" ]; then
        db_args="$db_args --skip business::flow::internal_control::db_tests"
    fi
    # shellcheck disable=SC2086
    ( cd "$ROOT/backend" && cargo test --lib --features test-hooks -- $db_args ) > "$db_log" 2>&1 \
        || db_rc=$?
    grep -E '^test result:' "$db_log" | sed 's/^/  /' || true
    if [ "$db_rc" -ne 0 ]; then
        echo "  ✗ 真库用例有失败（退出码 $db_rc）。失败清单：" >&2
        grep -E '^test .* FAILED' "$db_log" | sed 's/^/    /' >&2 || true
        echo "    完整日志：$db_log" >&2
        exit 1
    fi
    # 假绿检测：只认「库没配 / 连不上」这一类 SKIP —— 那等于一条断言都没跑。
    # 数据前提类 SKIP（「本库的 5602 仍是末级科目」「库缺部门种子」…）是**如实**报告环境差异，
    # 不判红，但要打出来让人看得见（别把它也当成「全绿」）。
    # ⚠ 不能锚 `^SKIP:`：`--nocapture` 下 libtest 把测试输出**接在 `test <name> ... ` 同一行**
    #   （实测 `test …countersign_all_excluded_fails_closed ... SKIP: 未设置 P0FIX_DB_URL`）。
    false_green="$(grep -E 'SKIP: (未设置 [A-Z0-9_]*DB_URL|[A-Z0-9_]*DB_URL 连接失败|set_db 失败)' "$db_log" \
        | sed 's/^.*SKIP: /SKIP: /' | sort | uniq -c | sort -rn || true)"
    if [ -n "$false_green" ]; then
        echo "  ✗ 有真库用例因「库没配 / 连不上」而 SKIP（libtest 记 passed = 假绿）：" >&2
        printf '%s\n' "$false_green" | sed 's/^/    /' >&2
        echo "    修法：三个库 URL（ERP_DB_URL / SYS_DB_URL / TASK_DB_URL）指同一个隔离库；" >&2
        echo "          要跑内控那组再加 P0FIX_DB_URL。完整日志：$db_log" >&2
        exit 1
    fi
    other_skips="$(grep -c 'SKIP:' "$db_log" || true)"
    false_green_n="$(grep -cE 'SKIP: (未设置 [A-Z0-9_]*DB_URL|[A-Z0-9_]*DB_URL 连接失败|set_db 失败)' "$db_log" || true)"
    other_skips=$(( ${other_skips:-0} - ${false_green_n:-0} ))
    if [ "${other_skips:-0}" -gt 0 ]; then
        echo "  ℹ 另有 $other_skips 条「数据前提缺失」类 SKIP（非假绿，如实报告环境差异）："
        grep 'SKIP:' "$db_log" | sed 's/^.*SKIP: /SKIP: /' | sort -u | head -5 | sed 's/^/    /'
    fi
    echo "  ✓ 真库用例全绿、0 条「库未配置」假绿 SKIP（日志：$db_log）"
else
    echo "==> 闸⓪：真库用例 —— 未启用（CHECK_WITH_DB=1 才跑）"
    echo "    ⚠ 本档不开时，全部真库用例都会打印 SKIP 并 return（libtest 记 passed）⇒"
    echo "      「全绿」不代表真库断言跑过；夹具污染 / 真库断言过期这类缺陷只能在本档发现。"
    echo "      提交前建议：CHECK_WITH_DB=1 CHECK_WITH_DB_FILTER=<受影响模块> bash scripts/check.sh"
fi

# ======================================================================
# 批次 B · 数据权限防漏门禁（架构文档 §2.4 的「闸 2」）
#
# 为什么放在最后：这两条门禁在 B5（存量登记表接入）完成前**必然**失败，
# 放在最后能让 fmt / clippy / check / typecheck 的结果先完整呈现 ——
# 否则一次门禁失败会把整个检查链的结果都遮掉，开发者无法判断自己是否引入了新问题。
#
# 两条门禁的口径（与架构文档 §2.4 逐字一致）：
#   闸①：分页只有一种正确写法（scope::paginate）→ 硬门禁，除白名单外必须 0 命中；
#   闸②：登记表的查询要么接入 scope::，要么写一行 `// SCOPE-EXEMPT: <理由>`
#        → 允许「带理由的显式豁免」，因为清理任务 / 聚合计数 / 扇出这类查询
#          **不该**注入 scope（注入了会静默少数据，比漏权限更危险）。
# ======================================================================

# 登记表清单（架构文档 §2.5）：这些表的查询必须有 scope:: 或 SCOPE-EXEMPT
#
# 2026-10-29 设计修正 A（docs/云枢设计修正补丁-20261029.md 修正 2）补入 C+ 任务管理 7 张表：
#   卡面点名的 4 张：biz_task / biz_task_comment / biz_task_attachment / biz_task_worklog
#   另外 3 张（biz_task_member / biz_task_dependency / biz_task_status_def）一并列入：
#     · biz_task_member 在 business/task/model.rs 被**直接查询** 2 处（「我参与」的 id 集合
#       + 协作人列表），本来就该受闸②约束；
#     · biz_task_dependency / biz_task_status_def 目前还没有 Rust 查询（T1 只建表），
#       但它们是按 task_id 挂在任务下的子表，将来一旦分页查询就得有 scope —— 提前列入，
#       避免「新代码没有对应门禁条目」这类静默缺口；确实不需要数据范围的查询写
#       `// SCOPE-EXEMPT: <理由>` 即可（status_def 是全局字典，属典型豁免场景）。
# 注：闸② 的表名判定是**前缀匹配**（`FROM biz_task` 本身就会命中 biz_task_* 全家族，
# 由 scripts/lint/scope_gate.py 用 `"FROM <t>" in line` 做子串判定，等价于原 `grep -rl`）；
# 7 条并列列出是为了让清单自解释，重复命中由脚本内的 set 收敛。
BATCHB_SCOPED_TABLES="sys_user sys_dept sys_import_task sys_export_task sys_notice
sys_file operation_logs audit_logs auth_logs security_logs
biz_task biz_task_comment biz_task_attachment biz_task_worklog
biz_task_member biz_task_dependency biz_task_status_def
biz_project biz_project_member
biz_flow_def biz_flow_def_version biz_flow_node biz_flow_edge biz_flow_node_assignee
biz_flow_instance biz_flow_instance_node biz_flow_task biz_flow_cc"

# 2026-10-29 T7c 补入 2 张项目表（`biz_project` / `biz_project_member`）：
#   · 卡面 t_b9f70dab「其他·12」明确要求 `biz_project` 进闸② 清单
#     （要么接 scope 要么写 SCOPE-EXEMPT 理由）；
#   · 两个模块都**接的是 scope**：`business/project/model.rs` 的项目分页走
#     `scope::paginate(PROJECT_TARGET, …)`、单条走 `scope::filter_for`；
#     `biz_task/model.rs` 里新增的 `LEFT JOIN biz_project pj` 只是展示字段，不产生新查询入口。
#   · 成员表 `biz_project_member` 一并列入（和 `biz_task_member` 同口径：它是按 project_id
#     挂在项目下的子表，一旦有人写「按 project_id 直查成员」的分页，就必须过门禁）。

# 2026-10-29 批次D · D1 补入 5 张审批流定义侧表（`biz_flow_def` / `biz_flow_def_version` /
# `biz_flow_node` / `biz_flow_edge` / `biz_flow_node_assignee`），卡面 t_a92e9d5e「其他·13」要求。
#   口径：**这 5 张表写 SCOPE-EXEMPT，不接 scope** —— 流程定义是**全局配置**
#   （「全平台共用的规则」，不是谁的业务数据）。按部门范围裁剪会导致
#   「A 部门的人看不到自己的单据要走哪个流程」；写权限由 `business:flow:create/update/delete/publish`
#   这 4 个权限码隔离，可见范围由统一门禁 `business:flow:read` 控制。
#   同一口径的先例：`biz_task_status_def`（全局字典）。豁免理由写在
#   `backend/src/business/flow/model.rs` 的文件头（`// SCOPE-EXEMPT: …` 两行），闸② 命中即放过。

# 2026-10-29 批次D · D2 补入 4 张审批流运行侧表（`biz_flow_instance` /
# `biz_flow_instance_node` / `biz_flow_task` / `biz_flow_cc`），卡面 t_3715c446「其他·26」要求。
#   口径：**运行侧接 scope**（与定义侧相反）—— 实例有 `initiator_dept_id` / `initiator_id`，
#   可见性是「数据范围 ∪ 我发起的 ∪ 我待办/已办 ∪ 抄送我」的并集，`flow/model.rs::page_instances`
#   用 `scope::filter_for(…, INSTANCE_TARGET)` 生成数据范围片段后与 id 集合做 OR（架构 §9.1 / D16）。
#   `biz_flow_task` / `biz_flow_cc` / `biz_flow_instance_node` 与实例同文件（`flow/model.rs`），
#   闸② 按**文件**判定，因此只要该文件里有 `scope::` 即整体通过；
#   `flow/timeout.rs` 里的到期扫描是后台任务，写了 `SCOPE-EXEMPT` 理由。

# 闸② 的 B0 实测基线（2026-10-28，本卡在 cf84231 树上实测）。
# 架构文档 §2.4 记的是 17 处——那是同一套脚本在更早的树上跑的；差异 4 处的来源
# 逐条列在 docs/批次B-B0B1-验证证据.md §3，不是口径放宽，而是把那 4 个
# file×table 组合也算进来了（附录 C 的 #9/#11/#24 本来就要求处理它们）。
BATCHB_GATE2_BASELINE=21

# 闸① 白名单：scope::paginate 自身的实现文件（B2 落地）
BATCHB_GATE1_WHITELIST="business/shared/scope.rs"

# warn 模式下用来记住「门禁按告警处理」的条数（0 = 没有告警）
BATCHB_GATE_WARNED=0

batchb_gates() {
    local src="$ROOT/backend/src"
    local failed=0
    local gate1_out gate1_n i t f gate2_n gate2_out

    echo "==> 批次B 闸①：分页必须走 scope::paginate（paginate_with_params 仅允许出现在白名单）"
    echo "    白名单：$BATCHB_GATE1_WHITELIST（+ 注释行不计）"
    # 注意写法：这里不能用 `! grep ...`（「找不到」时反而命中 → 门禁永远报错，
    # 是这类脚本最常见的自伤）；统一用变量 + 空判。
    gate1_out=$(grep -rn "paginate_with_params" "$src" --include=*.rs \
        | grep -v "$BATCHB_GATE1_WHITELIST" \
        | grep -v -E "^[^:]+:[0-9]+: *(//|\*)" || true)
    if [ -n "$gate1_out" ]; then
        gate1_n=$(printf '%s\n' "$gate1_out" | wc -l | tr -d ' ')
        echo "  FAIL：$gate1_n 处直接调 paginate_with_params（基线 20 处；B5 完成后应为 0）"
        printf '%s\n' "$gate1_out" | sed 's/^/    /'
        failed=$((failed + 1))
    else
        echo "  PASS：0 处（分页全部走 scope::paginate）"
    fi

    echo "==> 批次B 闸②：登记表的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由"
    echo "    登记表：$(echo "$BATCHB_SCOPED_TABLES" | tr '\n' ' ')"

    # 判定实现搬进 scripts/lint/scope_gate.py（门禁口径 v2 · D1，见 docs/门禁口径-v2-20261103.md）：
    #   · 只判定「会进生产编译的代码」—— 父模块以 `#[cfg(test)]` 声明的模块文件整文件跳过，
    #     文件内 `#[cfg(test)] mod xxx { … }` 内联区跳过；跳过可审计（打印计数，
    #     `python3 scripts/lint/scope_gate.py --list-skipped` 打印清单+声明位置）；
    #   · 原有语义逐条保留：整行注释不算代码行（`//`、`*` 开头）、`SCOPE-EXEMPT` 沿整文件搜、
    #     表名按前缀命中（`FROM biz_task` 也命中 biz_task_*）、按文件聚合计数；
    #   · 扫描范围硬断言 + 「已知生产文件必须在生产面」的防假绿断言都在脚本里：
    #     缺覆盖 / 解析器把生产文件误判成测试面 → 立刻 FAIL，不许静默变绿。
    gate2_n=0
    gate2_out=""
    if command -v python3 >/dev/null 2>&1; then
        if gate2_out="$(python3 "$ROOT/scripts/lint/scope_gate.py" \
            --root "$ROOT" --baseline "$BATCHB_GATE2_BASELINE" 2>&1)"; then
            printf '%s\n' "$gate2_out"
        else
            printf '%s\n' "$gate2_out"
            failed=$((failed + 1))
            gate2_n="$(printf '%s\n' "$gate2_out" | sed -n 's/^  FAIL：\([0-9][0-9]*\) 个文件.*$/\1/p' | head -1)"
            gate2_n="${gate2_n:-1}"
        fi
    else
        echo "  FAIL：找不到 python3（闸② 需要它按 Rust 模块语义解析测试面）"
        failed=$((failed + 1))
        gate2_n=1
    fi

    echo "==> 批次B 门禁汇总：闸①=${gate1_n:-0} 处  闸②=${gate2_n:-0} 处  （B0 基线 20 / ${BATCHB_GATE2_BASELINE}）"

    if [ "$failed" -gt 0 ]; then
        if [ "${BATCHB_GATE:-fail}" = "warn" ]; then
            echo "==> 批次B 门禁：$failed 条未通过（BATCHB_GATE=warn → 仅告警，不中断）" >&2
            BATCHB_GATE_WARNED="$failed"
            return 0
        fi
        echo "==> 批次B 门禁：$failed 条未通过。修法（二选一）：" >&2
        echo "    ① 分页接口改成 scope::paginate；单条/导出查询用 scope::filter_for" >&2
        echo "    ② 确实不需要数据权限的查询，在函数上方写一行 // SCOPE-EXEMPT: <理由>" >&2
        echo "    B2–B4 施工期可临时用 BATCHB_GATE=warn bash scripts/check.sh 绕过本门禁。" >&2
        return 1
    fi
    echo "==> 批次B 门禁：全部通过"
    return 0
}

# 批次B 两闸属 L2：闸② 度量的是「还没接入数据权限的存量查询」，存量未清零前在树上
# 就是红的（ARCH-9 §0 D-6「已知红台账」口径），不能进 pre-push —— 否则每次 push 都被
# 存量债挡住，钩子当天就会被 SKIP_PREPUSH 绕过，等于没有钩子。
if want L2; then
    batchb_gates
fi

# ======================================================================
# 批次 C · C7：变更日志「回滚零残留」六条门禁（G1–G6）
#
# 依据 docs/批次C-技术架构.md §4.12.4（G1–G5）与 §5.7 R14（G6）。
# 这六条在这里**只做 grep / 静态检查**（秒级），运行时行为由
# `scripts/verify/changelog_rollback.sh`（真库 + 真事务，`0|1|0`）与
# `cargo test --features test-hooks` 兜底。三者必须同时存在：
# 静态检查防「写错位置」，真库断言防「位置对了但语义错了」。
#
# 口径说明（为什么不按架构文档逐字照抄 grep）：
#   架构写的是 `grep -rn "db()" …`，而本仓库的 changelog 模块注释里大量出现
#   `db()`、`record(`（都是为了讲清「不要这么写」），照抄会把文档判红。
#   因此六条统一加一条「排除整行注释」的过滤（G2 的口径在 C7 卡面上已写明）
#   ——这是**排除注释**，不是放宽标准：任何真实代码命中仍然报红。
# ======================================================================

# 复用同一条「排除整行注释」的过滤：// 、//! 、/// 、* 开头都算注释
batchc_strip_comments() {
    grep -v -E '^[^:]+:[0-9]+:[[:space:]]*(//|\*)' || true
}

batchc_gates() {
    local src="$ROOT/backend/src"
    local failed=0
    local out n

    echo "==> C7 G1：只有 business/changelog/ 能碰 sys_change_log（防止别处偷偷 INSERT）"
    # 口径 v2（2026-11-10，卡 t_2746ca53）：判据搬进 scripts/lint/sys_change_log_gate.py，
    # **只判生产面**（测试文件整体跳过 + `#[cfg(test)]` 项按花括号配对跳过），与 scope_gate /
    # inventory_write_gate 同口径。v1 是纯 grep（只排注释），实测在 HEAD 上被测试代码里的
    # `SELECT … FROM sys_change_log` 断言误判成 2 处红 —— 测试读审计表不是生产写路径。
    # 判据没有放宽：生产面仍是 0 容忍，且新增 --self-test 红/绿语料守住判据本身。
    if command -v python3 >/dev/null 2>&1; then
        if ! python3 "$ROOT/scripts/lint/sys_change_log_gate.py" --self-test >/dev/null 2>&1; then
            echo "  FAIL：判据自检未通过（判据被改坏了？python3 scripts/lint/sys_change_log_gate.py --self-test）"
            failed=$((failed + 1))
        elif out="$(python3 "$ROOT/scripts/lint/sys_change_log_gate.py" --root "$ROOT" 2>&1)"; then
            echo "  PASS：生产面 0 处（测试面按语义豁免）"
        else
            printf '%s\n' "$out" | sed 's/^/    /'
            failed=$((failed + 1))
        fi
    else
        echo "  FAIL：找不到 python3（G1 需要它按 Rust 模块语义解析测试面）"
        failed=$((failed + 1))
    fi

    echo "==> C7 G2：changelog 模块内禁止自开事务（db() 非注释命中必须为 0）"
    out=$(grep -rn "db()" "$src/business/changelog/" --include=*.rs | batchc_strip_comments || true)
    if [ -n "$out" ]; then
        n=$(printf '%s\n' "$out" | wc -l | tr -d ' ')
        echo "  FAIL：$n 处（模块内只能吃 &impl SqlExecutor，不能 db()）"
        printf '%s\n' "$out" | sed 's/^/    /'
        failed=$((failed + 1))
    else
        echo "  PASS：0 处（注释中的示例不计）"
    fi

    echo "==> C7 G3：record 的签名必须带 &impl SqlExecutor"
    if awk '/^pub async fn record\(/{flag=1} flag{print; if ($0 ~ /\) ->/) exit}' \
        "$src/business/changelog/mod.rs" | grep -q "&impl SqlExecutor"; then
        echo "  PASS"
    else
        echo "  FAIL：mod.rs 里 pub async fn record( 之后没找到 &impl SqlExecutor"
        failed=$((failed + 1))
    fi

    echo "==> C7 G4：不得存在『无 executor 版本』的 record 函数"
    # 注意 `record(_in)?[[:space:]]*\(`：必须有词边界收尾，否则
    # `async fn record_then_rollback(`（C7 的测试钩子）会被误判成 record 家族成员。
    out=""
    while IFS=: read -r file line _rest; do
        [ -n "$file" ] || continue
        if ! awk -v start="$line" 'NR >= start && NR <= start + 20 {print; if ($0 ~ /\) ->/) exit}' "$file" \
            | grep -q "impl SqlExecutor"; then
            out="${out}${file}:${line}: async fn record* 缺 executor 参数"$'\n'
        fi
    done < <(grep -rnE "async fn record(_in)?[[:space:]]*\(" "$src/business/changelog/" --include=*.rs | batchc_strip_comments || true)
    if [ -n "$out" ]; then
        echo "  FAIL："
        printf '%s' "$out" | sed 's/^/    /'
        failed=$((failed + 1))
    else
        echo "  PASS"
    fi

    echo "==> C7 G5：每个 record/record_in 调用点必须在 transaction 闭包内且传 &tx"
    if command -v python3 >/dev/null 2>&1; then
        if ! python3 "$ROOT/scripts/lint/changelog_in_tx.py" --root "$ROOT"; then
            failed=$((failed + 1))
        fi
    else
        echo "  FAIL：找不到 python3（G5 需要它做词法检查）"
        failed=$((failed + 1))
    fi

    echo "==> C7 G6：按 id 查 sys_change_log 必须同时带分区键 created_at（R14）"
    out=$(grep -rn "FROM sys_change_log WHERE id" "$src" --include=*.rs | batchc_strip_comments || true)
    if [ -n "$out" ]; then
        n=$(printf '%s\n' "$out" | wc -l | tr -d ' ')
        echo "  FAIL：$n 处（主键含分区键，纯 id 查询会扫全部分区）"
        printf '%s\n' "$out" | sed 's/^/    /'
        failed=$((failed + 1))
    else
        echo "  PASS：0 处"
    fi

    echo "==> C7 门禁汇总：G1–G6 共 6 条，未通过 $failed 条"
    if [ "$failed" -gt 0 ]; then
        echo "==> C7 门禁：$failed 条未通过。修法：" >&2
        echo "    G1 sys_change_log 只能由 business/changelog/ 访问" >&2
        echo "    G2 changelog 模块内不要 db()，改用调用方传进来的 &impl SqlExecutor" >&2
        echo "    G3/G4 record 必须只吃 &impl SqlExecutor（不许有无 executor 的重载）" >&2
        echo "    G5 record(_in) 只能在 db().transaction(|tx| async move { … }) 里调用，第一个实参传 &tx" >&2
        echo "    G6 按 id 查必须带 created_at（分区键），否则会扫全部月分区" >&2
        return 1
    fi
    echo "==> C7 门禁：全部通过"
    return 0
}

if want L0; then
    batchc_gates
fi

# ======================================================================
# 批次 D · 偏好体系：字典 `ui.palette` 的 oneof 必须与前端色板逐字一致
#
# 漂移后果是静默故障：前端加了色板后端没加 → 「界面能选、保存被 400 拒绝」；
# 后端多留已删色板 → 库里存着前端不认识的值（刷新后配色被打回默认）。
# 这条检查秒级，放在所有编译类检查之后、汇总之前。
# ======================================================================

if want L0; then
echo "==> 偏好字典与色板一致性（V0091 ui.palette oneof ↔ style/palettes.ts）"
if ! python3 "$ROOT/scripts/lint/palette_def_sync.py" --root "$ROOT"; then
    echo "==> 一致性门禁未通过：改色板时必须同步改 V0091 的 validator" >&2
    exit 1
fi
fi

# ======================================================================
# 迁移文件名 / 版本号跨目录全局唯一（判据 A–E，见脚本头注释）
#
# 为什么是硬门禁：`backend/migrations/{system,support,business}` 在磁盘上是三个目录，
# 但 nik 的加载器把它们合成**一张槽位表**，再按版本号排序执行。同一个版本号出现在两个
# 目录里 ⇒ 加载阶段直接 `duplicate up migration for version N` + FATAL，**与库的状态无关**
# （2026-11-13 实测：主题配色卡 2609c7f6 与 HR-40 卡 83a4cfab 各取了一个 V0397，
# 空库首启 rc=1、已迁满 329 条记录的库重启也 rc=1）。各卡的真库回放脚本是
# `ls | sort -n | psql` 自己逐条回放的，**不经过加载器**，所以「319 条回放全绿」证明不了
# 版本号唯一 —— 只有静态检查能提前发现。
#
# 判定规则本身由 `--selftest` 的红/绿语料守住（语料被改坏 / 判据被放宽，自检立刻红）。
# ======================================================================

if want L0; then
echo "==> 迁移版本号门禁 · 判据自检（--selftest）"
if ! python3 "$ROOT/scripts/lint/migration_version_unique.py" --selftest; then
    echo "==> 门禁自身失效：自检语料未全绿（判据被改坏了？）" >&2
    exit 1
fi

echo "==> 迁移文件名与版本号（scripts/lint/migration_version_unique.py）"
if ! python3 "$ROOT/scripts/lint/migration_version_unique.py" --root "$ROOT"; then
    echo "==> 门禁未通过：版本号必须跨 system/support/business 全局唯一（取全局最大 + 1），" >&2
    echo "    且 up/down 成对、已应用的迁移不许改号（指纹校验会红）。" >&2
    exit 1
fi
fi

# ======================================================================
# 库存三本账「唯一写入口」闸（P2-5A）
#
# 为什么是硬门禁：`biz_inventory` / `biz_inventory_location` / `biz_inventory_txn`
# 收口前分别有 13 / 13 / 8 个文件直接写 SQL，同一件事各写一遍 —— `ROUND` 位数、
# 整桶归零、`GREATEST(…,0)` 夹 0 这些口径只要有一处漂移，账不会当场报错，
# 只会在巡检里变成「账不对」。写语句必须只在 `inventory_writer.rs` 里。
#
# 豁免：明确的测试文件命名（`*_tests.rs` / `tests.rs` / `test_*.rs` …）+ 文件内
# `#[cfg(test)]` 标记的项（花括号配对，只跳测试项本身，不再「到文件末尾全豁免」）+
# 注释。**"文件名恰好包含 test 子串"不再豁免**（`latest_stock.rs` 会被正常扫描）。
# 判定规则本身由 `--self-test` 的红/绿语料守住。
# ======================================================================

if want L0; then
echo "==> 库存三本账唯一写入口闸 · 判据自检（--self-test）"
if ! python3 "$ROOT/scripts/verify/inventory_write_gate.py" --self-test; then
    echo "==> 门禁自身失效：自检语料未全绿（判据被改坏了？）" >&2
    exit 1
fi

echo "==> 库存三本账唯一写入口（scripts/verify/inventory_write_gate.py）"
if ! python3 "$ROOT/scripts/verify/inventory_write_gate.py"; then
    echo "==> 门禁未通过：库存三本账的写语句必须走 backend/src/business/erp/inventory_writer.rs" >&2
    exit 1
fi
fi

# ======================================================================
# 批次 D · ERP 账务不变量（P9「闸③」）
#
# 为什么是硬门禁：ERP 的账错了不会抛异常，只会**安静地算错** —— 库存对不上总账、
# 单子作废了没退库存、过账了没生成凭证。这些只有「不变量体检」能发现，
# 任何一条 BLOCKER 都说明账已经错了，不许合。
#
# 三档开关（`ERP_INVARIANTS_GATE`）：
#   block（默认）= 有 BLOCKER → exit 1
#   warn         = 只告警（存量脏数据还没修完时临时用）
#   off          = 跳过（CI 没有数据库 / 本机没起库时用）
#
# 判定口径：脚本退出码（有 BLOCKER 时 exit 1）+ 汇总行里的 `BLOCKER N`。
# ======================================================================
erp_invariants_gate() {
    local mode="${ERP_INVARIANTS_GATE:-block}"
    if [ "$mode" = "off" ]; then
        echo "==> 闸③：ERP 不变量（ERP_INVARIANTS_GATE=off → 跳过）"
        return 0
    fi
    echo "==> 闸③：ERP 不变量（scripts/verify/erp_invariants.py）"
    if [ -f /etc/cloudpivot/cloudpivot.env ]; then
        # shellcheck disable=SC1091
        set -a; . /etc/cloudpivot/cloudpivot.env; set +a
    else
        echo "  ! 未找到 /etc/cloudpivot/cloudpivot.env：按环境变量 / 默认值连库"
    fi
    if [ -z "${NIK_DATABASE_PASSWORD:-}" ] && [ -z "${PGPASSWORD:-}" ]; then
        echo "  ⚠ 没有库口令（NIK_DATABASE_PASSWORD / PGPASSWORD 均为空）→ 视作「无 DB 环境」，跳过"
        echo "    （CI 里没有库是正常的；本机要跑就得先 . /etc/cloudpivot/cloudpivot.env）"
        return 0
    fi

    local log
    log="$(mktemp /tmp/erp_inv.XXXXXX.log)"
    if python3 "$ROOT/scripts/verify/erp_invariants.py" > "$log" 2>&1; then
        echo "  ✓ 全部不变量成立（$(grep -c '✅' "$log" || true) 条通过）"
        rm -f "$log"
        return 0
    fi

    local blockers
    blockers="$(grep -oE 'BLOCKER [0-9]+' "$log" | head -1 | grep -oE '[0-9]+' || true)"
    blockers="${blockers:-?}"
    if [ "$mode" = "warn" ]; then
        echo "  ⚠ 有 ${blockers} 条 BLOCKER（ERP_INVARIANTS_GATE=warn → 不阻断）"
        grep '^  · \[' "$log" | head -10 || true
        rm -f "$log"
        return 0
    fi
    echo "  ✗ 有 ${blockers} 条 BLOCKER，账已错，不许合：" >&2
    grep '^  · \[' "$log" | head -10 >&2 || true
    echo "    完整报告：$log（修完再跑；存量修复期可 ERP_INVARIANTS_GATE=warn 临时放行）" >&2
    return 1
}

if want L2; then
if ! erp_invariants_gate; then
    exit 1
fi
fi

# ======================================================================
# 闸④ 业务类型词表值域（P2-5 / ARCH-3 §5 DC-21）
#
# 为什么是硬门禁：词表漂移不会报错 —— 落一个新 `biz_type` 字面量，报表按类型分组就多一行、
# 巡检按类型 JOIN 就漏一类。唯一真相是 `backend/src/business/shared/doc_kind.rs` 的
# `COLUMN_DOMAINS`，本闸就是「拿词表去查库」。E2E 残留（`E2E_*` / `D2_SMOKE`）用 --strict 判红。
#
# 开关：`BIZ_VOCAB_GATE` = block（默认）/ warn / off
# ======================================================================
biz_vocab_gate() {
    local mode="${BIZ_VOCAB_GATE:-block}"
    if [ "$mode" = "off" ]; then
        echo "==> 闸④：业务类型词表（BIZ_VOCAB_GATE=off → 跳过）"
        return 0
    fi
    echo "==> 闸④：业务类型词表值域（scripts/verify/biz_type_domain.py --strict）"
    if [ -f /etc/cloudpivot/cloudpivot.env ]; then
        # shellcheck disable=SC1091
        set -a; . /etc/cloudpivot/cloudpivot.env; set +a
    fi
    if [ -z "${NIK_DATABASE_PASSWORD:-}" ] && [ -z "${PGPASSWORD:-}" ]; then
        echo "  ⚠ 没有库口令（NIK_DATABASE_PASSWORD / PGPASSWORD 均为空）→ 视作「无 DB 环境」，跳过"
        return 0
    fi
    local log rc
    log="$(mktemp /tmp/biz_vocab.XXXXXX.log)"
    python3 "$ROOT/scripts/verify/biz_type_domain.py" --strict > "$log" 2>&1
    rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "  ✓ 所有业务类型列的值都在 doc_kind.rs 词表域内"
        rm -f "$log"
        return 0
    fi
    if [ "$rc" -ge 2 ]; then
        echo "  ⚠ 巡检环境错误（退出码 $rc：连不上库）→ 跳过，不算失败"
        rm -f "$log"
        return 0
    fi
    if [ "$mode" = "warn" ]; then
        echo "  ⚠ 有域外值（BIZ_VOCAB_GATE=warn → 不阻断）"
        grep '✗' "$log" | head -10 || true
        rm -f "$log"
        return 0
    fi
    echo "  ✗ 词表域外值，不许合（改代码或补词表，别在库里留第 9 套词表）：" >&2
    grep '✗' "$log" | head -10 >&2 || true
    echo "    完整报告：$log" >&2
    return 1
}

# ======================================================================
# 闸⑤ 快照一致性巡检（B1 / DI-01..DI-26）
#
# 为什么是硬门禁：这 26 条查的是「快照字段与真相源漂移」——单据名改了快照没改、
# 多态引用指向已删单据、结构护栏（约束/唯一索引）被删。它们都不是「会抛异常」的错，
# 只会在报表/追溯上安静地错。BLOCKER 一条都不许合。
#
# `di` 视图缺失（退出码 2）= 本库没执行过 `snapshot_audit.py --install-views`，
# 属环境未准备，跳过并提示，不阻断。
# 开关：`SNAPSHOT_AUDIT_GATE` = block（默认）/ warn / off
# ======================================================================
snapshot_audit_gate() {
    local mode="${SNAPSHOT_AUDIT_GATE:-block}"
    if [ "$mode" = "off" ]; then
        echo "==> 闸⑤：快照一致性巡检（SNAPSHOT_AUDIT_GATE=off → 跳过）"
        return 0
    fi
    echo "==> 闸⑤：快照一致性巡检（scripts/verify/snapshot_audit.py）"
    if [ -f /etc/cloudpivot/cloudpivot.env ]; then
        # shellcheck disable=SC1091
        set -a; . /etc/cloudpivot/cloudpivot.env; set +a
    fi
    if [ -z "${NIK_DATABASE_PASSWORD:-}" ] && [ -z "${PGPASSWORD:-}" ]; then
        echo "  ⚠ 没有库口令 → 视作「无 DB 环境」，跳过"
        return 0
    fi
    local log rc
    log="$(mktemp /tmp/snapshot_audit.XXXXXX.log)"
    python3 "$ROOT/scripts/verify/snapshot_audit.py" > "$log" 2>&1
    rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "  ✓ 快照巡检 BLOCKER 0（$(grep -oE '巡检项 [0-9]+' "$log" | head -1)）"
        rm -f "$log"
        return 0
    fi
    if [ "$rc" -eq 2 ] || [ "$rc" -eq 3 ]; then
        echo "  ⚠ 巡检未执行（退出码 $rc：di 视图缺失 / 上一轮还在跑）→ 跳过，先跑 --install-views"
        rm -f "$log"
        return 0
    fi
    if [ "$mode" = "warn" ]; then
        echo "  ⚠ 有 BLOCKER（SNAPSHOT_AUDIT_GATE=warn → 不阻断）"
        grep '✗' "$log" | head -10 || true
        rm -f "$log"
        return 0
    fi
    echo "  ✗ 快照巡检有 BLOCKER，不许合：" >&2
    grep '✗' "$log" | head -10 >&2 || true
    echo "    完整报告：$log" >&2
    return 1
}

if want L2; then
if ! biz_vocab_gate; then
    exit 1
fi

if ! snapshot_audit_gate; then
    exit 1
fi
fi

# ======================================================================
# 闸⑥ 死权限码常驻闸（handlerCodes − menuAuthCodes 必须为空）
#
# 为什么是硬门禁：角色授权只能按 `sys_menu` 的行勾（`sys_role_menu` 挂的是 menu_id），
# 所以「后端 handler 写了 permission = "x"，但 sys_menu 里没有 auth_code = 'x' 的行」
# 等价于「这个功能除超管外谁都点不动」—— 不抛异常、不打日志，只在运营点按钮时才暴露。
# 2026-11-05 实测抓到 3 条（business:erp:voucher:audit / un-audit / post：财务经理
# 有 voucher:list 却永远审不了、过不了凭证），补齐后本闸就是「防再长」的那道。
#
# 口径（详见脚本头注释）：
#   missing_in_menu（代码在用、菜单没有）→ **必须 0**，否则 exit 1 并点名到文件行；
#   unused_in_code （菜单有、代码注解无）→ 只报告不判红：行级校验（claims.has_permission）
#                    与纯前端权限点（router meta.permission）都是合法形态。
# 白名单必须带理由（源码 `// PERM-EXEMPT: <理由>` 或脚本内 `EXEMPT = {码: 理由}`），
# 裸白名单 = 关闸，脚本直接判红。
#
# 开关：`PERM_MENU_GATE` = block（默认）/ warn / off
# 无库环境（NIK_DATABASE_PASSWORD / PGPASSWORD 均空）→ 按「无 DB 环境」跳过，
# 与闸③④⑤ 同口径（CI 里没有库是正常的；本机要跑就先 . /etc/cloudpivot/cloudpivot.env）。
# ======================================================================
perm_menu_gate() {
    local mode="${PERM_MENU_GATE:-block}"
    if [ "$mode" = "off" ]; then
        echo "==> 闸⑥：死权限码（PERM_MENU_GATE=off → 跳过）"
        return 0
    fi
    echo "==> 闸⑥：死权限码（scripts/verify/perm_menu_gate.py）"

    # 判据自检先跑（不连库，秒级）：判据被改坏时自检先红，不会「扫出 0 个码 → 假绿」。
    local selftest
    if ! selftest="$(python3 "$ROOT/scripts/verify/perm_menu_gate.py" --self-test 2>&1)"; then
        echo "  ✗ 闸门自检未通过（判据被改坏了？）：" >&2
        printf '%s\n' "$selftest" | grep -E '✘' | head -5 >&2
        return 1
    fi
    echo "  ✓ 判据自检通过（$(printf '%s\n' "$selftest" | grep -oE '[0-9]+/[0-9]+ 通过' | head -1)）"

    if [ -f /etc/cloudpivot/cloudpivot.env ]; then
        # shellcheck disable=SC1091
        set -a; . /etc/cloudpivot/cloudpivot.env; set +a
    fi
    if [ -z "${NIK_DATABASE_PASSWORD:-}" ] && [ -z "${PGPASSWORD:-}" ]; then
        echo "  ⚠ 没有库口令（NIK_DATABASE_PASSWORD / PGPASSWORD 均为空）→ 视作「无 DB 环境」，跳过"
        echo "    （本机要跑就先 . /etc/cloudpivot/cloudpivot.env；离线比对生产快照用 --menu-codes-file）"
        return 0
    fi

    local log rc
    log="$(mktemp /tmp/perm_menu.XXXXXX.log)"
    python3 "$ROOT/scripts/verify/perm_menu_gate.py" > "$log" 2>&1
    rc=$?
    if [ "$rc" -eq 0 ]; then
        local summary
        summary="$(grep -oE '注解码 [0-9]+ 个' "$log" | head -1)"
        echo "  ✓ missing_in_menu = 0（$summary 全部有菜单承载）"
        echo "    $(grep -oE 'unused_in_code  = [0-9]+' "$log" | head -1) 只报告不判红（行级校验 / 纯前端权限点）"
        rm -f "$log"
        return 0
    fi
    if [ "$rc" -ge 2 ]; then
        echo "  ⚠ 巡检环境错误（退出码 $rc：拿不到菜单权限码）→ 跳过，不算失败"
        sed 's/^/    /' "$log" | head -3
        rm -f "$log"
        return 0
    fi
    if [ "$mode" = "warn" ]; then
        echo "  ⚠ 有死权限码（PERM_MENU_GATE=warn → 不阻断）"
        awk '/✗ missing_in_menu/{f=1} /^ℹ unused_in_code/{f=0} f' "$log" | sed 's/^/    /' | head -12
        rm -f "$log"
        return 0
    fi
    echo "  ✗ 有权限码在后端被使用、但在 sys_menu 里没有承载行（除超管外无人可授），不许合：" >&2
    # 只摘 missing 那一段（`ℹ unused_in_code` 之后是只报告档，摘进来会稀释重点）
    awk '/✗ missing_in_menu/{f=1} /^ℹ unused_in_code/{f=0} f' "$log" | sed 's/^/    /' | head -20 >&2
    echo "    修法：补一条 sys_menu 行（迁移优先；生产补数走 scripts/datafix/…，固定 uuid + IF NOT EXISTS）；" >&2
    echo "          确实不该有菜单的，在注解上方写 // PERM-EXEMPT: <理由>。完整报告：$log" >&2
    return 1
}

if want L2; then
if ! perm_menu_gate; then
    exit 1
fi
fi

# ======================================================================
# 闸⑦ 孤儿按钮常驻闸（角色拿到的按钮码 ⇒ 至少有一个可达父页）
#
# 为什么是硬门禁：`sys_role_menu` 挂的是 `sys_menu` 的行，按钮（menu_type=3）挂在页面
# （menu_type=2）下。角色持有一个按钮码、却没有任何一个「带码祖先」被授给它 ⇒ 这个按钮
# 在权限树里挂在一个它永远看不见的页面下。2026-11-06 生产实测 8 对（FLOW_CONFIG_APPROVER /
# FINANCE_DIRECTOR × 4 个流程实例侧按钮，父页 business:flow:read 没授），根因是 V0124 写这
# 5 个按钮时目标页面还不存在（V0128 才种），之后没人归位 —— 修法见 V0259。
#
# 口径（与脚本头注释逐条一致，详见 scripts/verify/orphan_button_gate.py）：
#   orphan_pairs     = (角色,码)，该码的**每个**按钮节点向上走祖先链都有带码祖先未授 → **必须 0**；
#   redundant_node_grants = 同码另有可达节点、个别节点不可达 → 只报告（V0258 按码补授权会把
#                      该码的全部节点写进去，功能上无损失，判红只会逼人拆授权去凑绿）；
#   SUPER_ADMIN      = 不参与判红（*:*:* 通配，sys_role_menu 行对功能零影响，V0258 同口径）；
#   软删角色          = 不参与判红（登录不了），只报告。
# 防假绿：脚本自带扫描基数断言（活跃菜单 < 100 行 / 带码父页的按钮 < 50 个 ⇒ exit 2）
#         与判据自检（--self-test，不连库），判据被改坏时自检先红。
#
# 开关：`ORPHAN_BTN_GATE` = block（默认）/ warn / off
# 无库环境（NIK_DATABASE_PASSWORD / PGPASSWORD 均空）→ 按「无 DB 环境」跳过，与闸③④⑤⑥ 同口径。
# ======================================================================
orphan_button_gate() {
    local mode="${ORPHAN_BTN_GATE:-block}"
    if [ "$mode" = "off" ]; then
        echo "==> 闸⑦：孤儿按钮（ORPHAN_BTN_GATE=off → 跳过）"
        return 0
    fi
    echo "==> 闸⑦：孤儿按钮（scripts/verify/orphan_button_gate.py）"

    local selftest
    if ! selftest="$(python3 "$ROOT/scripts/verify/orphan_button_gate.py" --self-test 2>&1)"; then
        echo "  ✗ 闸门自检未通过（判据被改坏了？）：" >&2
        printf '%s\n' "$selftest" | grep -E '✘' | head -5 >&2
        return 1
    fi
    echo "  ✓ 判据自检通过（$(printf '%s\n' "$selftest" | grep -oE '[0-9]+/[0-9]+ 通过' | head -1)）"

    if [ -f /etc/cloudpivot/cloudpivot.env ]; then
        # shellcheck disable=SC1091
        set -a; . /etc/cloudpivot/cloudpivot.env; set +a
    fi
    if [ -z "${NIK_DATABASE_PASSWORD:-}" ] && [ -z "${PGPASSWORD:-}" ]; then
        echo "  ⚠ 没有库口令（NIK_DATABASE_PASSWORD / PGPASSWORD 均为空）→ 视作「无 DB 环境」，跳过"
        echo "    （本机要跑就先 . /etc/cloudpivot/cloudpivot.env；生产只读核对用 --remote root@172.18.10.144 --db cloudpivot）"
        return 0
    fi

    local log rc
    log="$(mktemp /tmp/orphan_btn.XXXXXX.log)"
    python3 "$ROOT/scripts/verify/orphan_button_gate.py" > "$log" 2>&1
    rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "  ✓ 孤儿 (角色,码) = 0（$(grep -oE '按钮授权 [0-9]+ 行' "$log" | head -1)）"
        grep -E '^ℹ' "$log" | sed 's/^/    /' | head -5
        rm -f "$log"
        return 0
    fi
    if [ "$rc" -ge 2 ]; then
        echo "  ⚠ 巡检环境错误（退出码 $rc：拿不到菜单/授权数据）→ 跳过，不算失败"
        sed 's/^/    /' "$log" | head -3
        rm -f "$log"
        return 0
    fi
    if [ "$mode" = "warn" ]; then
        echo "  ⚠ 有孤儿按钮（ORPHAN_BTN_GATE=warn → 不阻断）"
        grep -E '^  · ' "$log" | sed 's/^/    /' | head -12
        rm -f "$log"
        return 0
    fi
    echo "  ✗ 有按钮挂在该角色看不见的页面下（孤儿按钮），不许合：" >&2
    grep -E '^  · ' "$log" | sed 's/^/    /' | head -20 >&2
    echo "    修法：① 该角色确实该看该页面 ⇒ 补授父页码（新迁移，按码判重 + down）；" >&2
    echo "          ② 不该看 ⇒ 把按钮挂到别的父页（改 sys_menu.parent_id，零授权变化，如 V0259）。" >&2
    echo "    完整报告：$log" >&2
    return 1
}

if want L2; then
if ! orphan_button_gate; then
    exit 1
fi
fi

# ======================================================================
# 闸⑧ 迁移授权段角色码常驻闸（`INSERT INTO sys_role_menu` 里的 role_code 必须真实存在）
#
# 为什么是硬门禁：`sys_role_menu` 的授权是「角色 → 菜单行」，role_code 只是字符串字面量 ——
# 写错一个字母、或从老迁移里抄一份「历史角色码清单」，SQL 不会报错：
# `WHERE r.role_code IN ('ADMIN','ACCOUNTANT',…)` 一行都匹配不到，`INSERT … SELECT` 选中 0 行，
# 迁移照样成功、指纹照样记账，**授权等于从没发生过**。2026-11-06 实测：27 条迁移引用的码里
# ADMIN/ACCOUNTANT/FINANCE/TEST_UADMIN/BUYER/PURCHASER/WAREHOUSE_KEEPER/WAREHOUSE 在生产都不存在，
# 只有 SUPER_ADMIN 命中 —— 而超管的 `*:*:*` 通配掩盖了这个缺口（V0258 已把业务角色矩阵补齐）。
#
# 口径（详见 scripts/verify/role_code_gate.py 头注释）：
#   白名单 = 生产活角色（12 个，实测 `select role_code from sys_role where deleted_at is null`）
#            ∪ 迁移 `INSERT INTO sys_role` 种出来的角色（自动解析：新迁移可「先建角色再授」）；
#   历史基线 = scripts/verify/role_code_gate_baseline.json，**按 sha256 锁文件**：25 个存量文件
#            逐字不改 ⇒ 豁免（29 条授权语句）；改动历史文件 ⇒ sha 不符 ⇒ 全部重报（已应用迁移不可改）；
#   新增/改动文件的未知码 ⇒ 判红并点名 `文件:行`；确属历史个例可写 `-- ROLE-EXEMPT: <理由≥8 字>`；
#   策略提示（WARN，不判红）：基线冻结号 V0259 之后的迁移再写 `INSERT INTO sys_role_menu`
#            会提示「授权走 V0258 矩阵」，但**不影响退出码**。
#
# 防假绿：脚本自带 --self-test（SQL 解析 / 豁免 / 基线 sha / 防呆语料，29 条），
#         rc=2 = 环境错误（目录不存在 / 扫不到 .up.sql / 基线损坏）⇒ 这里按失败处理，不跳过。
# 开关：`ROLE_CODE_GATE` = block（默认）/ warn / off
# ======================================================================
role_code_gate() {
    local mode="${ROLE_CODE_GATE:-block}"
    if [ "$mode" = "off" ]; then
        echo "==> 闸⑧：迁移授权段角色码（ROLE_CODE_GATE=off → 跳过）"
        return 0
    fi
    echo "==> 闸⑧：迁移授权段角色码（scripts/verify/role_code_gate.py）"

    local selftest
    if ! selftest="$(python3 "$ROOT/scripts/verify/role_code_gate.py" --self-test 2>&1)"; then
        echo "  ✗ 闸门自检未通过（判据被改坏了？）：" >&2
        printf '%s\n' "$selftest" | grep -E '✘' | head -5 >&2
        return 1
    fi
    echo "  ✓ 判据自检通过（$(printf '%s\n' "$selftest" | grep -oE '[0-9]+/[0-9]+ 通过' | head -1)）"

    local log rc
    log="$(mktemp /tmp/role_code.XXXXXX.log)"
    python3 "$ROOT/scripts/verify/role_code_gate.py" > "$log" 2>&1
    rc=$?
    if [ "$rc" -eq 0 ]; then
        grep -E '^(ℹ|⚠)' "$log" | sed 's/^/  /' | head -3
        echo "  ✓ 无新增/改动的未知角色码（$(grep -oE '扫描面：[0-9]+' "$log" | head -1 | sed 's/扫描面：//') 个 .up.sql 已扫）"
        rm -f "$log"
        return 0
    fi
    if [ "$mode" = "warn" ]; then
        echo "  ⚠ 有未知角色码（ROLE_CODE_GATE=warn → 不阻断）"
        grep -E '^  · ' "$log" | sed 's/^/  /' | head -10 || true
        rm -f "$log"
        return 0
    fi
    echo "  ✗ 迁移里的 role_code 在 sys_role 里不存在（授权会静默 0 行），不许合：" >&2
    grep -E '^  · |^✗ ' "$log" | sed 's/^/  /' | head -20 >&2
    echo "    修法：① 换成生产真实角色码（select role_code from sys_role where deleted_at is null；" >&2
    echo "             见 docs/后端二开指南.md「迁移授权段怎么拿角色码」）；" >&2
    echo "          ② 新业务角色 ⇒ 先在迁移里 INSERT INTO sys_role 建出来再授；" >&2
    echo "          ③ 历史个例 ⇒ 上方 5 行内写注释  -- ROLE-EXEMPT: <理由≥8 字>。" >&2
    echo "    完整报告：$log" >&2
    return 1
}

if want L0; then
if ! role_code_gate; then
    exit 1
fi
fi

# ======================================================================
# L3 · 干净检出闸（卡 t_2746ca53 ②：禁止本机依赖）
#
# 评审 5.7 原文：「单测依赖 gitignored 私有配置，干净检出必红」。
# E2E-14 已把 `startup_guard.rs` 的断言改成「私有配置在就断言它，不在就断言 *.example.toml」，
# 但**没有门禁守住这条性质**：下一个人再写一条 `assert!(Path::new("config/config-dev.toml").exists())`
# 就会立刻把「干净检出必红」带回来，而且只有到 CI / 新人克隆时才炸。
#
# 这道闸的判据是**真的把干净检出做出来跑一遍**（不是模拟）：
#   ① `git archive HEAD | tar -x` → 只含 git 跟踪的文件（gitignored 的私有配置天然不在）；
#   ② 断言私有配置确实不在、模板 *.example.toml 确实在（否则说明 .gitignore 漏了 / 模板被删）；
#   ③ `cargo test --lib --no-run` 取已编译的 lib 单测二进制，把 **CWD 指到干净检出**再跑全量
#      （配置路径是 `config/` 相对 CWD，于是测试读到的是干净树里的配置面）；
#   ④ 断言 0 failed —— 红就是「干净检出必红」，必须修代码，不许改这道闸。
#
# 为什么能这么便宜（≈15 s）：复用主 target 里已编好的测试二进制，不重复编译；全量 4032 条
# 用例无库时 5.5 s 跑完（真库用例按设计 SKIP，库侧归闸⓪ 管）。
# 开关：CLEAN_CHECKOUT_GATE = block（默认）/ warn / off
# ======================================================================
clean_checkout_gate() {
    local mode="${CLEAN_CHECKOUT_GATE:-block}"
    if [ "$mode" = "off" ]; then
        echo "==> L3 干净检出闸（CLEAN_CHECKOUT_GATE=off → 跳过）"
        return 0
    fi
    local gate="$ROOT/scripts/verify/clean_checkout_gate.sh"
    if [ ! -f "$gate" ]; then
        echo "  ✗ 找不到 $gate" >&2
        return 1
    fi
    local rc=0
    bash "$gate" || rc=$?
    if [ "$rc" -eq 0 ]; then
        return 0
    fi
    if [ "$mode" = "warn" ]; then
        echo "  ⚠ 干净检出闸未通过（CLEAN_CHECKOUT_GATE=warn → 不阻断）" >&2
        return 0
    fi
    echo "  ✗ 干净检出闸未通过：干净树上有单测是红的（依赖了本机 gitignored 文件？）" >&2
    echo "    修法：测试要么断言入库的 *.example.toml，要么用 .exists() 兜底 —— 见 startup_guard.rs 的写法。" >&2
    return 1
}

if want L3; then
if ! clean_checkout_gate; then
    exit 1
fi
fi

# 批次B 的 warn 模式提示保留在最后（C7 门禁是硬门禁，没有 warn 开关）
if [ "$BATCHB_GATE_WARNED" -gt 0 ]; then
    echo "==> 检查完成（$LAYERS）：fmt / clippy / check / typecheck 通过；批次B 门禁 $BATCHB_GATE_WARNED 条按告警处理（BATCHB_GATE=warn）"
else
    echo "==> 全部检查通过（$LAYERS）"
fi
