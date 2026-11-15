-- ============================================================================
-- V0220__flow_standard_flag_fix.up.sql
--
-- 【P2-6a】把 2 条「名字自称『标准』、is_system 却是 FALSE」的预置流程定义改正。
--
-- # 病是什么（实测，不是笔误）
--
--   flow_code          flow_name                              is_system
--   contract_approve   合同审批（标准·金额分级）                  f   ← 应为 t
--   stock_take_post    盘点过账审批（标准·差异金额分级）            f   ← 应为 t
--
-- 这两行由系统预置（客户不该改图），却拿不到「标准」标签；而模板库查询
-- （`model::templates()`）又不过滤 `is_system`，于是它们以「模板」身份露出
-- 却标不出「标准」—— 用户自然分不清哪条是自己能改的。
--
-- # 根因：不是写错字，是**种子 INSERT 漏列**
--
--   `biz_flow_def.is_system` 由 V0193 加列，定义是
--   `BOOLEAN NOT NULL DEFAULT FALSE`（V0193__flow_def_is_system.up.sql:29）；
--   而这两条的种子 INSERT 列清单止于 `owner_dept_id`，没有 `is_system`：
--     · V0209__erp_contract_approval.up.sql:91-97
--     · V0211__erp_stock_take.up.sql:299-305
--   → 吃 DEFAULT FALSE。同类种子（po_approve / stock_in_post / …）由 V0193 §2
--   显式按 UUID 回填过，唯独这两条是 V0193 之后才种下的，没人再回填。
--
-- # 为什么**不改** V0209 / V0211（重要，别「顺手修」）
--
--   `schema_migrations` 对已应用的版本化迁移存**内容指纹**，改一个字下一次启动
--   就会以 `MigrationError::ChecksumMismatch` 拒绝启动（AGENTS.md 约定 6：
--   「已应用迁移指纹校验、不可改」）。历史文件是只读记录，缺陷只能由**新迁移**修正。
--
-- # 本迁移做什么
--
--   ① 按 `flow_code` 把这 2 行的 `is_system` 置 TRUE（幂等：`AND NOT is_system`，
--      重复执行 0 行变更）；
--   ② 自检：全库 `deleted_at IS NULL AND NOT is_system AND flow_name LIKE '%标准%'`
--      必须为 0，否则 RAISE EXCEPTION（迁移失败 → 启动失败，不会带病上线）。
--
--   `updated_at = now()` 与同类的 V0193 §2 保持一致（打标也是数据变更，审计要看得出
--   什么时候打的标）。副作用：流程定义页按 `updated_at DESC` 排序，这两行会短暂排到
--   最前 —— 一次性、无业务影响，比「改了内容却看不出来」可接受。
--
-- # 只动 2 行，不动别的
--
--   · 不做「名字含『标准』的一律打标」这种模糊匹配：客户另存为的副本可以自己起名叫
--     「我们的标准流程」，按名字改就把客户的流程标成系统预置了（那是翻不回来的越权）。
--     按 `flow_code` 精确圈定，只认种子里写死的这两个编码。
--   · 不动 `status` / `current_version_id` / 版本图：这是打标修复，不是流程改造。
--
-- # 号段提醒
--
--   ARCH-5（`docs/architecture/ARCH-5-工作流引擎设计.md` §18.3）计划把
--   V0220–V0229 留给工作流引擎（flow_type / 节点扩展 / trigger / 权限 …）。
--   本卡按父卡 `t_f0db09f3` 的指派占用 **V0220**（当时仓内最大已应用 = V0219）。
--   后续 ARCH-5 实施卡请从 **V0230** 起编号，避免与已应用的 V0220 撞号。
--
-- 回滚：`V0220__flow_standard_flag_fix.down.sql`（把 2 行改回 FALSE）
-- ============================================================================

DO $$
DECLARE
    n_fixed bigint;
    n_left  bigint;
BEGIN
    -- ① 打标（幂等：已经是 TRUE 的行不参与）
    UPDATE biz_flow_def
       SET is_system  = TRUE,
           updated_at = now()
     WHERE flow_code IN ('contract_approve', 'stock_take_post')
       AND deleted_at IS NULL
       AND NOT is_system;
    GET DIAGNOSTICS n_fixed = ROW_COUNT;

    -- ② 自检：修完之后，全库不许再有「名字自称标准、却非系统预置」的行。
    --    查全库而不是只查这 2 行 —— 目的就是把「下一个漏写 is_system 的种子迁移」也钉住，
    --    它一旦再犯，这里当场炸，而不是等用户在页面上发现标签不对。
    SELECT count(*)
      INTO n_left
      FROM biz_flow_def
     WHERE deleted_at IS NULL
       AND NOT is_system
       AND flow_name LIKE '%标准%';

    IF n_left <> 0 THEN
        RAISE EXCEPTION
            'V0220 自检失败：仍有 % 行「名称含『标准』但 is_system=FALSE」的流程定义（本迁移修正 % 行）；'
            '请先排查是不是又有种子 INSERT 漏写了 is_system',
            n_left, n_fixed;
    END IF;

    RAISE NOTICE 'V0220：is_system 打标 % 行（contract_approve / stock_take_post），'
                 '残留错标 % 行（应为 0）', n_fixed, n_left;
END $$;
