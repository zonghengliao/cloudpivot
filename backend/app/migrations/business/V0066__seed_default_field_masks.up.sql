-- ======================================================
-- V0066：列级脱敏 · 三条默认规则种子（批次 B · B6）
--
-- 依据：`docs/批次B-技术架构.md` §3.2（七种类型）与 §7 B6 行；
--       卡面验收第 5 条「V0066 种子：3 条默认规则存在」。
-- 回滚：V0066__seed_default_field_masks.down.sql（按固定 UUID 精确删除）
--
-- ── 三条规则 ────────────────────────────────────────────────
--   sys_user.mobile     类型 1（手机号）  参数 NULL → 缺省 "3,4" → 138****5678
--   sys_user.email      类型 3（邮箱）    无参数              → a***@mail.com
--   sys_user.real_name  类型 4（姓名）    参数 "1"            → 张**
--
-- `role_id = NULL` = 全局默认规则：对**所有**未豁免的人生效（超管 / 持
-- `system:field-mask:plain` 的人除外，见架构 §3.6）。
--
-- ── 为什么这三列 ────────────────────────────────────────────
-- 1. 它们正好是 `business/masking/user_export.rs` 里 `sensitive: true` 的三列 ——
--    导出的 fail-closed 闸（缺规则即 422）在**默认部署**下不会误伤正常导出；
-- 2. `sys_user` 是全系统最先被验证的表（`system/user/page` 是 B3 的数据权限样板），
--    用它做脱敏样板，运维一眼能对上号；
-- 3. 手机号 / 邮箱 / 姓名是《个人信息保护法》口径下最典型的个人信息字段。
--
-- ── 幂等 ────────────────────────────────────────────────────
-- 固定主键 + `ON CONFLICT DO NOTHING`：重复执行不报错，也不会覆盖运维后来调过的
-- 参数（`uk_sys_field_mask_rule` 是 (table_name, column_name, role_id) 唯一）。
-- ======================================================

INSERT INTO sys_field_mask (id, table_name, column_name, mask_type, mask_param, role_id,
                            status, reason)
VALUES
    ('b6000000-0000-4000-8000-000000000001'::uuid, 'sys_user', 'mobile', 1, NULL, NULL, 1,
     '个人信息保护：手机号属敏感个人信息，列表/详情/导出默认脱敏'),
    ('b6000000-0000-4000-8000-000000000002'::uuid, 'sys_user', 'email', 3, NULL, NULL, 1,
     '个人信息保护：邮箱属敏感个人信息，列表/详情/导出默认脱敏'),
    ('b6000000-0000-4000-8000-000000000003'::uuid, 'sys_user', 'real_name', 4, '1', NULL, 1,
     '个人信息保护：真实姓名保留姓氏，其余打码')
ON CONFLICT DO NOTHING;
