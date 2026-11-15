-- ============================================================================
-- V0391__flow_cost_settle_is_system.up.sql
--
-- 【ALERT t_e9d59061】给 V0381 种下的预置流程 `cost_settle_approve` 补上
-- `is_system = TRUE`。
--
-- # 病是什么（实测，2026-11-12 10:20 CST 定时巡检，144 生产库 cloudpivot）
--
--   flow_code             flow_name                     is_system
--   cost_settle_approve   成本结算审批（标准·两级）       f   ← 应为 t（V0381 种下）
--
--   巡检结论 FAIL total=81 passed=79 failed=2 blockers=1：
--     · BLOCKER = H6（静态扫迁移种子的 INSERT 列清单，不依赖库）
--     · WARN    = H5（库里名字自称「标准」却不是系统预置）
--   两者同根因，一条迁移同时消掉。
--
-- # 根因：与 V0220 / V0233 同一个错法 —— **种子 INSERT 漏列**，不是笔误
--
--   `biz_flow_def.is_system` 由 V0193 加列，定义是
--   `BOOLEAN NOT NULL DEFAULT FALSE`（V0193:29）；而 V0381 的种子 INSERT
--   列清单止于 `owner_dept_id`，没有 `is_system`：
--     · V0381__cost_flow_def.up.sql:154-157
--   → 吃 DEFAULT FALSE。
--
--   这已是同一缺陷的第三次复发（V0209/V0211 → V0228/V0231 → V0381），
--   正是 `scripts/verify/erp_invariants.py` 的 H6 存在的意义：**下一个漏列的
--   新种子不该靠人在模板库页面上发现少了几张卡**。
--
-- # 为什么**不改** V0381（重要，别「顺手修」）
--
--   `schema_migrations` 对已应用的版本化迁移存**内容指纹**（SHA-256，见
--   nik-db `migrate::history::checksum`），改一个字下次启动就以
--   ChecksumMismatch 拒绝启动（AGENTS.md 约定 6：已应用迁移不可改）。
--   历史文件是只读记录，缺陷只能由**新迁移**修正 —— 这就是本文件存在的唯一理由。
--
-- # 本迁移做什么
--
--   ① 按 `flow_code` 把这 1 行的 `is_system` 置 TRUE
--      （幂等：`AND NOT is_system`，重复执行 0 行变更）；
--   ② 自检一（fail-closed）：本行必须已经是 TRUE，否则 RAISE EXCEPTION；
--   ③ 自检二：预置白名单里不许有 `is_system = FALSE` 的行（口径同 V0233 §③），
--      用来钉住「下一个漏写 is_system 的种子」——那种迁移一旦上线，本迁移
--      之后的下一次启动会在自检处炸，而不是等用户发现模板少了。
--      ⚠️ 白名单只列「本迁移执行时已知」的预置编码（V0233 那 13 个 + 本卡的
--      cost_settle_approve），写宽不写窄：新增预置流程时**不需要**回来改它，
--      静态检查 H6 才是新种子的看门人。
--
--   `updated_at = now()` 与 V0193 §2 / V0220 / V0233 保持一致（打标也是数据
--   变更，审计要看得见）。副作用：流程定义页按 updated_at DESC 排序时这 1 行会
--   短暂排到最前 —— 一次性、无业务影响。
--
-- # 只按 flow_code 精确圈定，不做模糊匹配
--
--   不用「名字含『标准』一律打标」这类启发式：客户另存为的副本可以自己起名，
--   按名字改就把客户的流程标成系统预置了（翻不回来的越权）。
--
-- # 号段
--
--   动手前查过 `ls backend/migrations/*/`（业务最大 V0390 cost_menu_group；
--   全局无 V0391），本迁移取 **V0391**（版本号跨目录全局唯一）。
--
-- 回滚：`V0391__flow_cost_settle_is_system.down.sql`（把这一行改回 FALSE）
-- ============================================================================

DO $$
DECLARE
    n_fixed  bigint;
    n_left   bigint;
    v_bad    text;
    -- 截至 V0391，已知的预置流程编码（V0233 的白名单 + 本卡补的 1 个）。
    v_preset constant text[] := ARRAY[
        'task_accept',         -- V0127
        'stock_transfer',      -- V0157
        'stock_in_cancel',     -- V0174
        'payable_pay',         -- V0175
        'po_approve',          -- V0182
        'stock_in_post',       -- V0182
        'payable_cancel',      -- V0191
        'voucher_reverse',     -- V0191
        'flow_edge_config',    -- V0206
        'contract_approve',    -- V0209（V0220 补标）
        'stock_take_post',     -- V0211（V0220 补标）
        'budget_approve',      -- V0228（V0233 补标）
        'asset_disposal',      -- V0231（V0233 补标）
        'cost_settle_approve'  -- V0381（本迁移补标）
    ];
BEGIN
    -- ① 打标（幂等：已经是 TRUE 的行不参与）
    UPDATE biz_flow_def
       SET is_system  = TRUE,
           updated_at = now()
     WHERE flow_code = 'cost_settle_approve'
       AND deleted_at IS NULL
       AND NOT is_system;
    GET DIAGNOSTICS n_fixed = ROW_COUNT;

    -- ② 自检一：本迁移点名的这一行必须已经 TRUE（否则 UPDATE 白跑了，别带病上线）
    SELECT count(*)
      INTO n_left
      FROM biz_flow_def
     WHERE flow_code = 'cost_settle_approve'
       AND deleted_at IS NULL
       AND NOT is_system;
    IF n_left <> 0 THEN
        RAISE EXCEPTION
            'V0391 自检失败：cost_settle_approve 仍有 % 行为 is_system=FALSE（本迁移修正 % 行）',
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
            'V0391 自检失败：预置流程仍被标成客户自建（is_system=FALSE）：%。'
            '几乎可以断定是某个种子迁移的 INSERT 漏写了 is_system —— '
            '请新开一条迁移按 flow_code 补 is_system=TRUE（不许改已应用迁移），'
            '并在种子里显式写列（见 docs/后端二开指南.md §九 / erp_invariants H6）',
            v_bad;
    END IF;

    RAISE NOTICE 'V0391：is_system 打标 % 行（cost_settle_approve），预置白名单 % 个编码全部为系统流程',
                 n_fixed, array_length(v_preset, 1);
END $$;
