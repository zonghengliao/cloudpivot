#!/usr/bin/env bash
# 提交前检查：后端 fmt / clippy / check + 前端 typecheck + 批次B 数据权限防漏门禁
#
# 用法：bash scripts/check.sh
#
#   BATCHB_GATE=warn bash scripts/check.sh
#       批次B 两条门禁只告警不失败（B2–B4 施工期用；B5 完成后默认行为即为全绿）
#
# 说明：本脚本的退出码 = 「任一检查失败」。批次B 门禁是**故意**会在当前树上报错的
#       —— 它度量的是「还没接入数据权限的存量查询」，见 docs/批次B-DDL.md §6 与
#       docs/后端二开指南.md「数据权限检查清单」。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "==> 后端：cargo fmt --check"
(cd "$ROOT/backend" && cargo fmt -- --check)

echo "==> 后端：cargo clippy -- -D warnings"
(cd "$ROOT/backend" && cargo clippy --all-targets -- -D warnings)

echo "==> 后端：cargo check"
(cd "$ROOT/backend" && cargo check)

echo "==> 前端：pnpm typecheck"
(cd "$ROOT/frontend" && pnpm typecheck)

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
# 注：闸② 用 `grep -rl "FROM $t"` 做**前缀匹配**，`biz_task` 本身就会命中
# biz_task_* 全家族；7 条并列列出是为了让清单自解释，重复命中由下面的 `sort -u` 收敛。
BATCHB_SCOPED_TABLES="sys_user sys_dept sys_import_task sys_export_task sys_notice
sys_file operation_logs audit_logs auth_logs security_logs
biz_task biz_task_comment biz_task_attachment biz_task_worklog
biz_task_member biz_task_dependency biz_task_status_def"

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
    local gate1_out gate1_n i t f gate2_n gate2_tmp file tables

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

    # 扫描范围自检（设计修正 A · 修正 2 第②条）：
    # 闸② 的扫描根是**整个** backend/src（不是只扫 business/ 或某几个模块），
    # 因此 business/task/ 天然在范围内。但「天然在范围内」这种结论会随着目录搬家
    # 静默失效（模块改名 / 挪到别处 / 被 .gitignore 掉），所以这里把「必须在范围内
    # 且真有 .rs 文件」写成硬断言：宁可门禁自己红，也不要在缺少覆盖时假绿。
    local missing=0 d n_rs code_lines
    for d in business/task business system support shared; do
        if [ ! -d "$src/$d" ]; then
            echo "  FAIL：扫描范围缺少目录 backend/src/$d（闸② 会漏掉该目录下的查询）"
            missing=$((missing + 1))
            continue
        fi
        n_rs=$(find "$src/$d" -name '*.rs' -type f | wc -l | tr -d ' ')
        if [ "$n_rs" -eq 0 ]; then
            echo "  FAIL：扫描范围 backend/src/$d 下没有 .rs 文件（目录搬走或未纳入版本管理）"
            missing=$((missing + 1))
        fi
    done
    if [ "$missing" -eq 0 ]; then
        echo "  PASS：扫描范围已覆盖 backend/src/{business/task,business,system,support,shared}"
    else
        failed=$((failed + 1))
    fi

    gate2_tmp="$(mktemp)"
    # 注意写法：不用 `grep -l | while read`（子 shell 里的退出码/计数传不出来），
    # 改用 for 遍历 + 临时文件累计，计数与输出都在同一 shell 内。
    #
    # 判定**只看真实代码行**（整行注释先剥掉），两侧都剥：
    #   · `FROM <表>`：文档注释里举例的 SQL（如 business/shared/mod.rs 的 `//!` 示例）
    #     不是查询，不该把文件拖进门禁；
    #   · `scope::`：注释里写一句「本文件参考 `scope::ScopeKind`」就能让整个文件被判成
    #     已接入 —— 这类**假绿**比漏接更危险，它让门禁永远绿（skill cailu-dev §9.15-6 实测踩过）。
    # SCOPE-EXEMPT 相反：它本身就是注释标记，必须整文件搜。
    for t in $BATCHB_SCOPED_TABLES; do
        for f in $(grep -rl "FROM $t" "$src" --include=*.rs 2>/dev/null || true); do
            code_lines="$(mktemp)"
            grep -v -E '^[[:space:]]*(//|\*)' "$f" >"$code_lines" || true
            if grep -q "FROM $t" "$code_lines" \
                && ! grep -q "SCOPE-EXEMPT" "$f" \
                && ! grep -q "scope::" "$code_lines"; then
                # 只记「文件|表」原始对，展示与计数在下面按文件聚合
                printf '%s|%s\n' "${f#"$src"/}" "$t" >>"$gate2_tmp"
            fi
            rm -f "$code_lines"
        done
    done
    # 按**文件**聚合：一个文件漏接多张表只算 1 处（`biz_task` 会前缀命中
    # biz_task_member 等，同一文件因此可能出现多行），表名并列显示。
    gate2_n=0
    if [ -s "$gate2_tmp" ]; then
        while IFS= read -r file; do
            tables=$(grep -F "${file}|" "$gate2_tmp" | cut -d'|' -f2 | sort -u | tr '\n' ' ')
            printf '    FAIL: %s 查询 %s未接入 scope:: 且无 SCOPE-EXEMPT\n' "$file" "$tables"
            gate2_n=$((gate2_n + 1))
        done < <(cut -d'|' -f1 "$gate2_tmp" | sort -u)
    fi
    if [ "$gate2_n" -gt 0 ]; then
        echo "  FAIL：$gate2_n 个文件（按文件计；B0 实测基线 ${BATCHB_GATE2_BASELINE} 处是 file×table 口径，仅作历史参考）"
        failed=$((failed + 1))
    else
        echo "  PASS：0 处"
    fi
    rm -f "$gate2_tmp"

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

batchb_gates

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
    out=$(grep -rn "sys_change_log" "$src" --include=*.rs | grep -v "/business/changelog/" | batchc_strip_comments || true)
    if [ -n "$out" ]; then
        n=$(printf '%s\n' "$out" | wc -l | tr -d ' ')
        echo "  FAIL：$n 处（应在 0 处；变更日志只能由 changelog 模块写）"
        printf '%s\n' "$out" | sed 's/^/    /'
        failed=$((failed + 1))
    else
        echo "  PASS：0 处"
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

batchc_gates

# ======================================================================
# 批次 D · 偏好体系：字典 `ui.palette` 的 oneof 必须与前端色板逐字一致
#
# 漂移后果是静默故障：前端加了色板后端没加 → 「界面能选、保存被 400 拒绝」；
# 后端多留已删色板 → 库里存着前端不认识的值（刷新后配色被打回默认）。
# 这条检查秒级，放在所有编译类检查之后、汇总之前。
# ======================================================================

echo "==> 偏好字典与色板一致性（V0091 ui.palette oneof ↔ style/palettes.ts）"
if ! python3 "$ROOT/scripts/lint/palette_def_sync.py" --root "$ROOT"; then
    echo "==> 一致性门禁未通过：改色板时必须同步改 V0091 的 validator" >&2
    exit 1
fi

# 批次B 的 warn 模式提示保留在最后（C7 门禁是硬门禁，没有 warn 开关）
if [ "$BATCHB_GATE_WARNED" -gt 0 ]; then
    echo "==> 检查完成：fmt / clippy / check / typecheck 通过；批次B 门禁 $BATCHB_GATE_WARNED 条按告警处理（BATCHB_GATE=warn）"
else
    echo "==> 全部检查通过"
fi
