-- ============================================================================
-- V0233__flow_seed_is_system_fix2.up.sql
--
-- 【P2-6b-follow】把 FIN-1 / FIN-2 新种下的 2 条预置流程补上 `is_system = TRUE`。
--
-- # 病是什么（实测，2026-11-05 01:2x CST，库 cloudpivot_manual 跑到 V0232）
--
--   flow_code        flow_name                            is_system
--   budget_approve   预算审批（编制 / 调整双分支）            f   ← 应为 t（V0228 种下）
--   asset_disposal   资产处置审批（出售 / 报废 / 盘亏）        f   ← 应为 t（V0231 种下）
--
-- # 根因：与 V0220 同一个错法 —— **种子 INSERT 漏列**，不是笔误
--
--   `biz_flow_def.is_system` 由 V0193 加列，定义是
--   `BOOLEAN NOT NULL DEFAULT FALSE`（V0193:29）；而这两条的种子 INSERT 列清单
--   止于 `owner_dept_id`，没有 `is_system`：
--     · V0228__erp_budget_approval_flow.up.sql:47-50
--     · V0231__erp_asset_disposal_flow.up.sql:32-35
--   → 吃 DEFAULT FALSE。V0193 之后新增的预置流程（V0209/V0211/V0228/V0231）
--   各犯一次，说明这是**流程性缺陷**：靠人记「记得写 is_system」记不住。
--
-- # 为什么**不改** V0228 / V0231（重要，别「顺手修」）
--
--   `schema_migrations` 对已应用的版本化迁移存**内容指纹**（nik-db
--   `migrate::history::checksum`），改一个字下次启动就会以 ChecksumMismatch
--   拒绝启动（AGENTS.md 约定 6：已应用迁移不可改）。历史文件是只读记录，
--   缺陷只能由**新迁移**修正 —— 这就是本文件存在的唯一理由。
--
-- # 本迁移做什么
--
--   ① 按 `flow_code` 把这 2 行的 `is_system` 置 TRUE
--      （幂等：`AND NOT is_system`，重复执行 0 行变更）；
--   ② 自检一（fail-closed）：这 2 行必须已经是 TRUE，否则 RAISE EXCEPTION；
--   ③ 自检二（更广的口径）：**全库预置编码白名单里的行都不许 `is_system = FALSE`**。
--      白名单 = 截至本迁移所有由种子种下的 flow_code（见下方 v_preset 注释）。
--      它把「下一个漏写 is_system 的种子」也钉在这一步：那种迁移一旦上线，
--      本迁移的自检会在它之后的下一次启动里炸（迁移失败 → 启动失败），
--      而不是等用户在模板库页面上发现少了几张卡。
--      注意这是**库内**兜底；代码侧还有一条不依赖库状态的静态检查
--      （`scripts/verify/erp_invariants.py` 的 H6，扫 migrations 的 INSERT 列清单）。
--
--   `updated_at = now()` 与 V0193 §2 / V0220 保持一致（打标也是数据变更，审计要看得见）。
--   副作用：流程定义页按 `updated_at DESC` 排序，这 2 行会短暂排到最前 —— 一次性、无业务影响。
--
-- # 只按 flow_code 精确圈定，不做模糊匹配
--
--   不用「名字含『标准』/『审批』一律打标」这类启发式：客户另存为的副本可以自己起名，
--   按名字改就把客户的流程标成系统预置了（那是翻不回来的越权）。同理不碰
--   V0228/V0231 种的版本图、状态、hash —— 这是打标修复，不是流程改造。
--
-- # 号段
--
--   动手前查过 `ls backend/migrations/*/`（业务最大 V0232；system 侧在途 V0225
--   已占号），本迁移取 **V0233**（版本号跨目录全局唯一）。
--
-- 回滚：`V0233__flow_seed_is_system_fix2.down.sql`（把这 2 行改回 FALSE）
-- ============================================================================

DO $$
DECLARE
    n_fixed  bigint;
    n_left   bigint;
    v_bad    text;
    -- 截至 V0233，所有由迁移种下的预置流程编码（种子 INSERT 的 flow_code）。
    -- 新增预置流程时**故意不要求**回来改这个数组：静态检查 H6 才是新种子
    -- 的看门人；这里的口径是「本迁移执行时已知的预置集合」，写宽不写窄。
    v_preset constant text[] := ARRAY[
        'task_accept',       -- V0127
        'stock_transfer',    -- V0157
        'stock_in_cancel',   -- V0174
        'payable_pay',       -- V0175
        'po_approve',        -- V0182
        'stock_in_post',     -- V0182
        'payable_cancel',    -- V0191
        'voucher_reverse',   -- V0191
        'flow_edge_config',  -- V0206
        'contract_approve',  -- V0209（V0220 补标）
        'stock_take_post',   -- V0211（V0220 补标）
        'budget_approve',    -- V0228（本迁移补标）
        'asset_disposal'     -- V0231（本迁移补标）
    ];
BEGIN
    -- ① 打标（幂等：已经是 TRUE 的行不参与）
    UPDATE biz_flow_def
       SET is_system  = TRUE,
           updated_at = now()
     WHERE flow_code IN ('budget_approve', 'asset_disposal')
       AND deleted_at IS NULL
       AND NOT is_system;
    GET DIAGNOSTICS n_fixed = ROW_COUNT;

    -- ② 自检一：本迁移点名的这 2 行必须已经 TRUE（否则 UPDATE 白跑了，别带病上线）
    SELECT count(*)
      INTO n_left
      FROM biz_flow_def
     WHERE flow_code IN ('budget_approve', 'asset_disposal')
       AND deleted_at IS NULL
       AND NOT is_system;
    IF n_left <> 0 THEN
        RAISE EXCEPTION
            'V0233 自检失败：budget_approve / asset_disposal 仍有 % 行为 is_system=FALSE（本迁移修正 % 行）',
            n_left, n_fixed;
    END IF;

    -- ③ 自检二：预置白名单里不许有非系统行（抓「下一个漏写 is_system 的种子」）
    SELECT string_agg(flow_code || '(' || flow_name || ')', '、' ORDER BY flow_code)
      INTO v_bad
      FROM biz_flow_def
     WHERE deleted_at IS NULL
       AND NOT is_system
       AND flow_code = ANY (v_preset);
    IF v_bad IS NOT NULL THEN
        RAISE EXCEPTION
            'V0233 自检失败：预置流程仍被标成客户自建（is_system=FALSE）：%。'
            '几乎可以断定是某个种子迁移的 INSERT 漏写了 is_system —— '
            '请新开一条迁移按 flow_code 补 is_system=TRUE（不许改已应用迁移），'
            '并在种子里显式写列（见 docs/后端二开指南.md §九 / erp_invariants H6）',
            v_bad;
    END IF;

    RAISE NOTICE 'V0233：is_system 打标 % 行（budget_approve / asset_disposal），预置白名单 % 个编码全部为系统流程',
                 n_fixed, array_length(v_preset, 1);
END $$;
