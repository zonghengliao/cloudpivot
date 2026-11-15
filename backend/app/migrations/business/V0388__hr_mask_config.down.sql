-- ============================================================
-- V0388 回滚：撤掉 2 条 HR 脱敏规则 + 3 条 HR 配置（HR-05）
--
-- 顺序（**先护栏、后删、最后断言零残留**）：
--   ① 护栏：未删员工里还有身份证 / 工资卡数据时 **拒绝回滚**
--   ② 按固定主键删 2 条脱敏规则（`b8800000-…0001` / `…0002`）
--   ③ 按 config_key 删 3 条配置（含运维改过的行 —— 回滚的语义就是撤掉这套配置）
--   ④ 断言零残留（缺一条就报错，避免「迁移记了个成功但库里还留着」）
--
-- 为什么 ① 是硬护栏（不是「提示一下」）：
--   这两条规则是 `biz_employee.id_card / bank_account` 唯一的全局脱敏入口。员工主档里存的是**明文**
--   （导入查重 / 社保申报 / 代发工资要用），规则一删，列表页、详情页、导出**立刻**把身份证与工资卡
--   明文吐出来 —— 那是《个人信息保护法》口径下的事故，不是「回滚没回干净」。要回滚，先确认业务
--   已下线或先把这两列清空（`UPDATE biz_employee SET id_card = NULL, bank_account = NULL`）。
--
-- 幂等：DELETE 不存在的行是 0 行影响，不报错；结尾断言只核「该没的都没了」。
-- 文件内**不写** BEGIN/COMMIT（迁移器自己在事务里跑）。
-- ============================================================

-- ============================================================
-- §1 护栏：有员工身份数据就拒绝
-- ============================================================
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.biz_employee') IS NULL THEN
        RAISE NOTICE 'V0388 回滚：biz_employee 不存在（HR-01 已先回滚？），跳过数据护栏';
        RETURN;
    END IF;

    SELECT count(*) INTO n
      FROM biz_employee
     WHERE deleted_at IS NULL
       AND (id_card IS NOT NULL OR bank_account IS NOT NULL);
    IF n > 0 THEN
        RAISE EXCEPTION
            'V0388 回滚被拒绝：还有 % 名未删员工存着身份证或工资卡号。删掉脱敏规则会让这些明文直接出现在列表/详情/导出里；请先确认业务已下线，或清空 biz_employee.id_card / bank_account 再回滚', n;
    END IF;

    RAISE NOTICE 'V0388 回滚护栏通过：没有未删员工持有身份证/工资卡数据';
END $$;

-- ============================================================
-- §2 删 2 条脱敏规则（按 up 里写死的固定主键精确删，不按 (表,列) 通配）
-- ============================================================
-- 为什么按 id 而不是按 `table_name='biz_employee' AND role_id IS NULL`：
--   运维可能给同一列加过**角色级**规则（那是合法的并存），通配删会把它们连坐；
--   也可能把全局规则的 mask_param 调过 —— 那行 id 不变，照样按 id 删（回滚就是要撤掉它）。
DELETE FROM sys_field_mask
 WHERE id IN ('b8800000-0000-4000-8000-000000000001'::uuid,
              'b8800000-0000-4000-8000-000000000002'::uuid);

-- ============================================================
-- §3 删 3 条配置（按 config_key；软删过的也一并清掉，避免留下「键被占但读不到」的行）
-- ============================================================
DELETE FROM sys_config
 WHERE config_key IN ('hr.salary.iit_mode',
                      'hr.salary.si_mode',
                      'hr.attendance.require_leave_match')
   AND category = 'hr';

-- ============================================================
-- §4 零残留断言
-- ============================================================
DO $$
DECLARE
    n integer;
BEGIN
    SELECT count(*) INTO n
      FROM sys_field_mask
     WHERE id IN ('b8800000-0000-4000-8000-000000000001'::uuid,
                  'b8800000-0000-4000-8000-000000000002'::uuid);
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0388 回滚失败：仍有 % 条脱敏规则残留（主键 b8800000-…0001/0002）', n;
    END IF;

    SELECT count(*) INTO n
      FROM sys_config
     WHERE config_key IN ('hr.salary.iit_mode',
                          'hr.salary.si_mode',
                          'hr.attendance.require_leave_match');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0388 回滚失败：仍有 % 条 hr 配置残留（含软删行）', n;
    END IF;

    RAISE NOTICE 'V0388 回滚完成：2 条 HR 脱敏规则 + 3 条 hr 配置已全部清除';
END $$;
