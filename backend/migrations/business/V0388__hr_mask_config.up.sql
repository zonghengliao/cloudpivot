-- ============================================================
-- V0388：人力资源 · 敏感字段脱敏登记（2 条）+ 3 个 HR 开关（HR-05）
--
-- 依据：docs/architecture/ARCH-9-人力资源模块设计.md
--   §3.2 ⑥（2 条全局脱敏：biz_employee.id_card / biz_employee.bank_account）
--   §3.2 ⑩（3 条 sys_config：hr.salary.iit_mode / hr.salary.si_mode / hr.attendance.require_leave_match）
--   §7.1（迁移清单）、§8.1（拆卡清单 HR-05）
--
-- ── 号段（为什么不是卡面写的 V0382）────────────────────────────
-- 设计号 V0378~V0388 已被成本核算实占（V0378__cost_menu_and_perm … V0382__stock_in_production_sourceless），
-- 本仓迁移版本号**跨目录全局唯一**（重号 ⇒ 服务启动失败，见 AGENTS.md 约定 6）。HR 全系列两次顺延后：
--   V0383 hr_tables / V0384 hr_accounts / V0385 cost_module_switch /
--   V0386 hr_seq_dict / V0387 hr_menu_perms / **V0388 hr_mask_config（本文件）** / V0389 hr_attachment_doc_types
--
-- ── 这两条脱敏为什么必须种（不是「顺手加两行配置」）─────────────
-- 1. `biz_employee.id_card` / `bank_account` **存明文**（导入查重、社保申报、代发工资要用），
--    出参侧靠 `sys_field_mask` 脱敏。不登记 = 员工档案页 / 导出直接把身份证与工资卡明文吐出去。
-- 2. 两个 `mask_param` 与 `business/masking/masker.rs` 的解析口径逐字对齐：
--      mask_type=2（身份证）+ '6,4' → 110101****1234   （保留前 6 后 4，中间固定 4 个星号）
--      mask_type=5（银行卡）+ '4,4' → 6222 **** 7890   （保留前 4 后 4）
--    留空也会走缺省参数（身份证/银行卡的缺省同为 4,4），显式写死是为了「配置页上看得见口径」。
--    ⚠ 参数写错不是「脱敏变弱」而是 **fail-closed**：脱敏函数报错时整值抹成掩码（masker 的硬约定）。
-- 3. `role_id IS NULL` = 全局默认规则，对所有未豁免的人生效（超管 / 持 `system:field-mask:plain`
--    的人除外）。明文通道（`business:hr:employee:sensitive`）的判定在 HR-11/HR-18 的 DTO 层，不在本迁移。
--
-- ── 三个开关为什么现在种 ────────────────────────────────────
-- 一期个税 / 社保公积金**手工录入**（设计 D-9），二期切自动只改配置（MANUAL → AUTO）不动代码；
-- 先种下去，配置页才查得到「这两个值将来会变自动」。`hr.attendance.require_leave_match` 是
-- 考勤导入的校验开关（HR-30~HR-33 读取）。
--
-- ── 幂等 ────────────────────────────────────────────────────
--   · 脱敏：固定主键 + `NOT EXISTS (table_name, column_name, role_id IS NULL, deleted_at IS NULL)`。
--     业务唯一键是 `uk_sys_field_mask_rule (table_name, column_name, role_id) NULLS NOT DISTINCT
--     WHERE deleted_at IS NULL`（V0062），重放时命中 NOT EXISTS 直接跳过 —— **不覆盖**运维后来调过的
--     `mask_param` / `status`。`ON CONFLICT DO NOTHING` 只兜底「主键被别人手搓占用」。
--   · 配置：固定主键 + `NOT EXISTS (config_key, deleted_at IS NULL)`（`uk_sys_config_key` 是部分唯一索引）。
--     重放不覆盖运维改过的 `config_value`（例如已切 AUTO）。
--   · 文件内**不写** BEGIN/COMMIT（迁移器自己在事务里跑）。
--
-- ── 回滚 ────────────────────────────────────────────────────
--   `V0388__hr_mask_config.down.sql`：按固定主键删 2 条脱敏、按 config_key 删 3 条配置；
--   **未删员工里还有身份证/工资卡数据时拒绝回滚**（删掉脱敏规则 = 明文直接出系统边界）。
-- ============================================================

-- ============================================================
-- §1 前置条件（配了不生效比不配更糟：规则必须落在真实存在的表与列上）
-- ============================================================
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.sys_field_mask') IS NULL THEN
        RAISE EXCEPTION 'V0388 前置条件失败：sys_field_mask 表不存在（V0062 未应用？）';
    END IF;
    IF to_regclass('public.sys_config') IS NULL THEN
        RAISE EXCEPTION 'V0388 前置条件失败：sys_config 表不存在（support/V0009 未应用？）';
    END IF;
    IF to_regclass('public.biz_employee') IS NULL THEN
        RAISE EXCEPTION 'V0388 前置条件失败：biz_employee 表不存在（HR-01 / V0383 未应用？）——脱敏规则必须登记在真实存在的表上，否则永远不生效且没人报错';
    END IF;

    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_employee'
       AND column_name IN ('id_card', 'bank_account');
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0388 前置条件失败：biz_employee 上只找到 % 个待脱敏列（id_card / bank_account 应各 1）', n;
    END IF;

    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_sys_field_mask_rule';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0388 前置条件失败：唯一索引 uk_sys_field_mask_rule 不存在（本迁移的幂等依赖它）';
    END IF;

    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_sys_config_key';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0388 前置条件失败：唯一索引 uk_sys_config_key 不存在（本迁移的幂等依赖它）';
    END IF;

    RAISE NOTICE 'V0388 前置条件通过：sys_field_mask / sys_config / biz_employee(id_card,bank_account) 与两个唯一索引在位';
END $$;

-- ============================================================
-- §2 脱敏登记：身份证 / 工资卡（全局规则，role_id = NULL）
--
-- 主键用固定 UUID（`b8800000-…` 段，实测 0 行占用），这样 down 能精确按 id 删，
-- 且重复应用不会插出第二条同 (表,列,NULL) 的行（那会撞 uk_sys_field_mask_rule 的 NULLS NOT DISTINCT）。
-- ============================================================
INSERT INTO sys_field_mask (id, table_name, column_name, mask_type, mask_param, role_id, status, reason)
SELECT 'b8800000-0000-4000-8000-000000000001'::uuid, 'biz_employee', 'id_card', 2, '6,4', NULL, 1,
       '个人信息保护：身份证号默认脱敏（明文需 business:hr:employee:sensitive）'
 WHERE NOT EXISTS (
        SELECT 1 FROM sys_field_mask
         WHERE table_name = 'biz_employee' AND column_name = 'id_card'
           AND role_id IS NULL AND deleted_at IS NULL)
ON CONFLICT DO NOTHING;

INSERT INTO sys_field_mask (id, table_name, column_name, mask_type, mask_param, role_id, status, reason)
SELECT 'b8800000-0000-4000-8000-000000000002'::uuid, 'biz_employee', 'bank_account', 5, '4,4', NULL, 1,
       '个人信息保护：工资卡号默认脱敏（明文需 business:hr:employee:sensitive）'
 WHERE NOT EXISTS (
        SELECT 1 FROM sys_field_mask
         WHERE table_name = 'biz_employee' AND column_name = 'bank_account'
           AND role_id IS NULL AND deleted_at IS NULL)
ON CONFLICT DO NOTHING;

-- ============================================================
-- §3 三个 HR 开关（category='hr'，config_level=1 系统级）
--
-- `config_level` 是 SMALLINT（1 系统级 / 2 业务级），`config_type` 用大写枚举 STRING/BOOLEAN/NUMBER/JSON ——
-- 设计阶段的证据脚本第一版写成 'SYSTEM'/'string'，真库直接 `invalid input syntax for type smallint`
-- （设计 §3.2 ⑩ 末尾记着这条实测）。这里按实测类型写。
-- ============================================================
INSERT INTO sys_config (id, config_key, config_value, config_type, description, category,
                        sort_order, config_level, is_modifiable, is_encrypted, is_restart, status)
SELECT 'b8800000-0000-4000-8000-000000000011'::uuid, 'hr.salary.iit_mode', 'MANUAL', 'STRING',
       '个税算法：MANUAL 手工录入（一期）/ AUTO 累计预扣法（二期）',
       'hr', 10, 1, TRUE, FALSE, FALSE, 1
 WHERE NOT EXISTS (SELECT 1 FROM sys_config WHERE config_key = 'hr.salary.iit_mode' AND deleted_at IS NULL)
ON CONFLICT DO NOTHING;

INSERT INTO sys_config (id, config_key, config_value, config_type, description, category,
                        sort_order, config_level, is_modifiable, is_encrypted, is_restart, status)
SELECT 'b8800000-0000-4000-8000-000000000012'::uuid, 'hr.salary.si_mode', 'MANUAL', 'STRING',
       '社保公积金算法：MANUAL 手工录入（一期）/ AUTO 按城市规则（二期）',
       'hr', 20, 1, TRUE, FALSE, FALSE, 1
 WHERE NOT EXISTS (SELECT 1 FROM sys_config WHERE config_key = 'hr.salary.si_mode' AND deleted_at IS NULL)
ON CONFLICT DO NOTHING;

INSERT INTO sys_config (id, config_key, config_value, config_type, description, category,
                        sort_order, config_level, is_modifiable, is_encrypted, is_restart, status)
SELECT 'b8800000-0000-4000-8000-000000000013'::uuid, 'hr.attendance.require_leave_match', 'TRUE', 'BOOLEAN',
       '导入考勤时是否校验「无薪假 ≤ 已批准请假天数」',
       'hr', 30, 1, TRUE, FALSE, FALSE, 1
 WHERE NOT EXISTS (SELECT 1 FROM sys_config WHERE config_key = 'hr.attendance.require_leave_match' AND deleted_at IS NULL)
ON CONFLICT DO NOTHING;

-- ============================================================
-- §4 自检（up 后必须全为真；重放时也必须成立 —— 幂等的判据在这里，不靠「看着没报错」）
-- ============================================================
DO $$
DECLARE
    n      integer;
BEGIN
    -- 4.1 两条脱敏规则在位且**取值精确**（mask_type / mask_param 写错等于脱敏口径变了）
    SELECT count(*) INTO n
      FROM sys_field_mask
     WHERE table_name = 'biz_employee' AND role_id IS NULL AND deleted_at IS NULL
       AND status = 1
       AND ((column_name = 'id_card'      AND mask_type = 2 AND mask_param = '6,4')
         OR (column_name = 'bank_account' AND mask_type = 5 AND mask_param = '4,4'));
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0388 断言 A 失败：biz_employee 的全局脱敏规则应 2 条且取值精确，实际 % 条（重复应用不会覆盖运维改过的值，此处只做口径核对）', n;
    END IF;

    -- 4.2 不许有第二种全局规则（同一列两条全局 = 唯一索引该拦，拦不住说明索引被删）
    SELECT count(*) INTO n
      FROM sys_field_mask
     WHERE table_name = 'biz_employee' AND role_id IS NULL AND deleted_at IS NULL;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0388 断言 B 失败：biz_employee 全局脱敏规则应恰好 2 条，实际 % 条', n;
    END IF;

    -- 4.3 登记的列在目标表上真实存在（配了不生效的根源）
    SELECT count(*) INTO n
      FROM sys_field_mask m
      JOIN information_schema.columns c
        ON c.table_schema = 'public' AND c.table_name = m.table_name AND c.column_name = m.column_name
     WHERE m.table_name = 'biz_employee' AND m.role_id IS NULL AND m.deleted_at IS NULL;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0388 断言 C 失败：登记的脱敏列只有 % 个能在 biz_employee 上找到（应 2）', n;
    END IF;

    -- 4.4 mask_param 形状必须能过解析器（"保留前N,保留后M"）；形状错的参数会让整值被抹掉
    SELECT count(*) INTO n
      FROM sys_field_mask
     WHERE table_name = 'biz_employee' AND role_id IS NULL AND deleted_at IS NULL
       AND (mask_param IS NULL OR mask_param !~ '^[0-9]+,[0-9]+$');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0388 断言 D 失败：有 % 条规则的 mask_param 不是 "前N,后M" 形状', n;
    END IF;

    -- 4.5 三条配置在位、类型/层级/分类正确（config_level 误写字符串在这里就炸）
    SELECT count(*) INTO n
      FROM sys_config
     WHERE category = 'hr' AND deleted_at IS NULL
       AND config_level = 1 AND status = 1 AND config_type IN ('STRING', 'BOOLEAN')
       AND ((config_key = 'hr.salary.iit_mode'                   AND config_value = 'MANUAL' AND config_type = 'STRING'  AND sort_order = 10)
         OR (config_key = 'hr.salary.si_mode'                    AND config_value = 'MANUAL' AND config_type = 'STRING'  AND sort_order = 20)
         OR (config_key = 'hr.attendance.require_leave_match'    AND config_value = 'TRUE'   AND config_type = 'BOOLEAN' AND sort_order = 30));
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0388 断言 E 失败：hr.* 配置应 3 条且取值精确，实际 % 条', n;
    END IF;

    -- 4.6 三条配置键不重复（重放插入第二遍）
    SELECT count(*) INTO n
      FROM (SELECT config_key FROM sys_config
             WHERE config_key IN ('hr.salary.iit_mode', 'hr.salary.si_mode', 'hr.attendance.require_leave_match')
               AND deleted_at IS NULL
             GROUP BY config_key HAVING count(*) > 1) d;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0388 断言 F 失败：有 % 个 hr 配置键重复（幂等失效）', n;
    END IF;

    RAISE NOTICE 'V0388 完成：biz_employee 2 条全局脱敏（id_card 2/6,4、bank_account 5/4,4）+ 3 条 hr 配置（iit_mode / si_mode / require_leave_match）';
END $$;
