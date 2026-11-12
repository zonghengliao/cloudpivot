-- ============================================================================
-- scripts/verify/hr_invariants.sql —— 人力资源模块数据一致性巡检（ARCH-9-2 §4.9）
--
-- 卡：t_914199ef（HR-45）。依据：docs/architecture/ARCH-9-人力资源模块设计.md
--   §4.9「巡检断言（10 条）」+ §7.1（收口自检分工）+ §4.1/§4.6/§4.9 各口径。
--
-- # 它做什么
--   10 条断言（A1~A10：8 条 BLOCKER + A4/A5 两条 WARN）+ 2 条 WARN 观察项（W1/W2），**只读**（无 INSERT/UPDATE/DELETE/DDL，
--   只建一张 pg_temp 临时表存结果）。可反复跑：生产、演练库、CI 都能跑。
--
-- # 与 V0398__hr_selfcheck 的分工
--   V0398 = 一次性静态种子完整性（10 表/科目/号段/菜单/流程/薪资项种子在不在）。
--   本脚本 = 可反复跑的**数据层面**不变式（单据算得对不对、凭证挂没挂、状态有没有死单）。
--   两者互补，别用本脚本替代迁移自检：库里没数据时本脚本全绿是正常的（0 行 = 无违规）。
--
-- # 怎么跑
--   bash scripts/verify/hr_invariants.sh                # 推荐（自动解析连接串 + 翻译退出码）
--   psql -v ON_ERROR_STOP=1 -f scripts/verify/hr_invariants.sql   # 裸跑：有 BLOCKER 就 RAISE
--   只报告不拦（自检/演练用）：PGOPTIONS='-c hr.inv.report_only=1' psql -f …
--
-- # 退出码（经 hr_invariants.sh）
--   0 = 全绿（8 条 BLOCKER 零违规，WARN 只打印）｜ 1 = 有 BLOCKER 违规｜ 2 = 环境问题
--
-- # 10 条断言（设计 §4.9 逐条落地，编号一致，便于设计文档 ↔ 脚本互查）
--   A1 BLOCKER  biz_employee.user_id 非空的行必须指向未软删的 sys_user，且 user_type='HUMAN'
--                （机器人账号不许挂员工档：自助接口会把 HR 数据发给 AI Actor）
--   A2 BLOCKER  同一 user_id 挂两份未删员工档 = 0 行（否则「我是谁」不可判定，工资发重）
--   A3 BLOCKER  同一 (employee_id, item_code) 的薪资标准生效区间不重叠
--                （重叠 ⇒ 同期取到两个标准，谁后被 ORDER BY 选中就成了随机数）
--   A4 WARN     status ∈ {PROBATION, ACTIVE} 的员工必须有覆盖今天的 ACTIVE 合同
--                （WARN：批量迁移上线的头几天可能还没补录合同，不该拦住出表）
--   A5 WARN     合同 end_date 早于员工 leave_date 的不一致行 = 0
--                （合同不该在离职后才到期；WARN：可能是先签长期合同后提前离职的正常单）
--   A6 BLOCKER  单头 ↔ 明细对账：gross_total = Σ detail.gross_amount 且
--                net_total = Σ detail.net_amount（工资表对外报的就是单头，错一分就是错）
--   A7 BLOCKER  明细 ↔ 明细项对账：Σ item(EARNING) − Σ item(PRE_DEDUCT) = detail.gross_amount
--                且 Σ item(WITHHOLD) = detail.withhold_amount（公式解释层与汇总层必须自洽）
--   A8 BLOCKER  status ∈ {POSTED, REVERSED} 的薪资单必须有 accrual/payment 两张凭证，
--                凭证存在、借贷平衡、source_type 分别是 PAYROLL_ACCRUAL / PAYROLL_PAYMENT
--                （红冲单同样要查：红字凭证不冲掉原凭证，两张原凭证仍在，A8 恒成立）
--   A9 BLOCKER  已过账薪资单的凭证 period 必须在 biz_accounting_period 有行；
--                且「已结账期间里不许存在 SUBMITTED/APPROVED 的薪资单」（死单）
--   A10 BLOCKER 请假单：status=SUBMITTED ⇒ 有 approval_instance_id 且实例仍 PENDING；
--                status=APPROVED ⇒ 有实例且实例已 APPROVED（含免审直过分支）
--   W1 WARN     考勤守恒：actual + 带薪假 + 无薪假 + 病假 + 旷工 ≈ should_days（±0.01）
--   W2 WARN     账号部门与员工人事部门漂移（借调合法，只报不拦，设计 §4.9 头注）
--
-- # 两处与设计字面的偏差（都是「照字面写会造成永久误报」，已写进卡内汇报）
--   ① A9 设计原文「凭证 period 必须存在且 status <> 'CLOSED'」：结账是**正常经营动作**，
--      11 月账结掉之后那张 11 月工资凭证就永远是 CLOSED 期间里的凭证 ⇒ 巡检永久红，
--      没人会再看它（狼来了）。设计意图是「防止结账后还能改数」，故改成两条等价的强化：
--        a) 凭证期间必须存在（否则凭证挂在黑期间，报表/账龄全都查不到）；
--        b) 已结账期间里不许再有 SUBMITTED/APPROVED 的薪资单 —— 这些单**永远过不了账**
--           （过账的期间闸会拒 CLOSED），属于必须人工处理的死单。
--   ② A10 设计原文「APPROVED 的必须有已通过的节点」：请假提交时若流程里没有审批节点
--      （免审配置 / 全跳过），引擎在同一事务里直接把实例推成 APPROVED 并把单据推到
--      APPROVED，此时**可能一个 APPROVED 节点行都没有**（leave_handler 的 auto_finished
--      分支）。照字面写会把这条合法路径判红。故断言实例终态 = APPROVED。
--
-- # 金额比较为什么带 0.005 容差
--   薪资项在公式里已 `ROUND(…, 2)`；单据列是 NUMERIC(18,4)。两侧都是定点数，理论上
--   恒等。留半分钱容差是为了容下未来「单头按 2 位小数落库」的口径调整，同时任何真实
--   差异（一分钱以上）照样报红 —— 造错自检里用的偏差是 100 元级，牙齿没有钝。
-- ============================================================================

\set ON_ERROR_STOP on
\pset footer off

-- ---------------------------------------------------------------------------
-- 0) 前置：10 张 HR 表必须都在（否则下面的断言全都会因缺表而报 SQL 错，
--    提示信息还不如直接说「先跑迁移」）
-- ---------------------------------------------------------------------------
DO $$
DECLARE v_missing text;
BEGIN
    SELECT string_agg(t, ' / ') INTO v_missing
      FROM unnest(ARRAY['biz_employee','biz_employee_contract','biz_employee_salary',
                        'biz_attendance_period','biz_attendance_record','biz_leave_request',
                        'biz_salary_item','biz_salary_sheet','biz_salary_detail',
                        'biz_salary_detail_item']) AS t
     WHERE to_regclass('public.' || t) IS NULL;
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'HR 巡检前置失败：这些表不存在 → %（先把迁移跑到 V0398）', v_missing;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- 1) 结果表（pg_temp：只在本次会话可见，会话结束自动消失，零残留）
--    `CREATE TEMP IF NOT EXISTS` + `TRUNCATE` 而不是 `DROP`：DROP 在 pg_temp 尚未
--    建出来时会打印 "schema pg_temp does not exist, skipping" 的 NOTICE，噪音。
-- ---------------------------------------------------------------------------
CREATE TEMP TABLE IF NOT EXISTS hr_inv_result (
    no     text PRIMARY KEY,
    level  text NOT NULL,
    name   text NOT NULL,
    viol   bigint NOT NULL,
    sample text
) ON COMMIT PRESERVE ROWS;
TRUNCATE hr_inv_result;

-- ---------------------------------------------------------------------------
-- 2) A1：员工档绑定的登录账号必须存在、未软删、user_type='HUMAN'
-- ---------------------------------------------------------------------------
INSERT INTO hr_inv_result
SELECT 'A1', 'BLOCKER',
       '员工档 user_id 必须指向未软删的 HUMAN 账号',
       count(*),
       left(coalesce(min(e.emp_no || '→user=' || e.user_id
                          || CASE WHEN u.id IS NULL THEN '（账号不存在/已删）'
                                  ELSE '（user_type=' || coalesce(u.user_type, '<空>') || '）' END), ''), 200)
  FROM biz_employee e
  LEFT JOIN sys_user u ON u.id = e.user_id AND u.deleted_at IS NULL
 WHERE e.deleted_at IS NULL
   AND e.user_id IS NOT NULL
   AND (u.id IS NULL OR coalesce(u.user_type, 'HUMAN') <> 'HUMAN');

-- ---------------------------------------------------------------------------
-- 3) A2：同一 user_id 挂两份未删员工档
-- ---------------------------------------------------------------------------
INSERT INTO hr_inv_result
SELECT 'A2', 'BLOCKER',
       '同一 user_id 只能挂一份未删员工档',
       count(*),
       left(coalesce(min(d.user_id::text || '（' || d.dup || ' 份）'), ''), 200)
  FROM (SELECT user_id, count(*) AS dup
          FROM biz_employee
         WHERE deleted_at IS NULL AND user_id IS NOT NULL
         GROUP BY user_id
        HAVING count(*) > 1) d;

-- ---------------------------------------------------------------------------
-- 4) A3：同一员工同一薪资项的生效区间不许重叠
--    daterange(…, '[]') ⇒ 闭区间；effective_to IS NULL ⇒ 上界无界（至今有效）
-- ---------------------------------------------------------------------------
INSERT INTO hr_inv_result
SELECT 'A3', 'BLOCKER',
       '同一员工同一薪资项的生效区间不重叠',
       count(*),
       left(coalesce(min(a.employee_id::text || '/' || a.item_code
                          || '：' || a.effective_from || '~' || coalesce(a.effective_to::text, '至今')
                          || ' ∩ ' || b.effective_from || '~' || coalesce(b.effective_to::text, '至今')), ''), 200)
  FROM biz_employee_salary a
  JOIN biz_employee_salary b
    ON b.employee_id = a.employee_id
   AND b.item_code   = a.item_code
   AND b.id > a.id
   AND b.deleted_at IS NULL
   AND daterange(a.effective_from, a.effective_to, '[]')
    && daterange(b.effective_from, b.effective_to, '[]')
 WHERE a.deleted_at IS NULL;

-- ---------------------------------------------------------------------------
-- 5) A4（WARN）：在职/试用期员工必须有覆盖今天的 ACTIVE 合同
-- ---------------------------------------------------------------------------
INSERT INTO hr_inv_result
SELECT 'A4', 'WARN',
       '在职/试用期员工应有覆盖今天的 ACTIVE 合同',
       count(*),
       left(coalesce(min(e.emp_no || '/' || e.status || '/入职 ' || e.hire_date), ''), 200)
  FROM biz_employee e
 WHERE e.deleted_at IS NULL
   AND e.status IN ('PROBATION', 'ACTIVE')
   AND NOT EXISTS (
        SELECT 1 FROM biz_employee_contract c
         WHERE c.employee_id = e.id
           AND c.deleted_at IS NULL
           AND c.status = 'ACTIVE'
           AND c.start_date <= CURRENT_DATE
           AND (c.end_date IS NULL OR c.end_date >= CURRENT_DATE));

-- ---------------------------------------------------------------------------
-- 6) A5（WARN）：合同 end_date 早于员工 leave_date
-- ---------------------------------------------------------------------------
INSERT INTO hr_inv_result
SELECT 'A5', 'WARN',
       '合同到期日不应早于员工离职日',
       count(*),
       left(coalesce(min(e.emp_no || '/合同 ' || c.contract_no || ' 止 ' || c.end_date
                          || ' < 离职 ' || e.leave_date), ''), 200)
  FROM biz_employee_contract c
  JOIN biz_employee e ON e.id = c.employee_id AND e.deleted_at IS NULL
 WHERE c.deleted_at IS NULL
   AND c.end_date IS NOT NULL
   AND e.leave_date IS NOT NULL
   AND c.end_date < e.leave_date;

-- ---------------------------------------------------------------------------
-- 7) A6：单头 ↔ 明细对账（设计 §4.6 三方对账的单头侧）
-- ---------------------------------------------------------------------------
INSERT INTO hr_inv_result
SELECT 'A6', 'BLOCKER',
       '单头 gross_total/net_total 必须等于明细汇总',
       count(*),
       left(coalesce(min(x.sheet_no
                          || '：单头 g=' || x.gross_total || ' n=' || x.net_total
                          || ' / 明细 g=' || x.detail_gross || ' n=' || x.detail_net), ''), 200)
  FROM (SELECT s.sheet_no, s.gross_total, s.net_total,
               coalesce(d.g, 0) AS detail_gross,
               coalesce(d.n, 0) AS detail_net
          FROM biz_salary_sheet s
          LEFT JOIN (SELECT sheet_id, sum(gross_amount) AS g, sum(net_amount) AS n
                       FROM biz_salary_detail GROUP BY sheet_id) d ON d.sheet_id = s.id
         WHERE s.deleted_at IS NULL) x
 WHERE abs(x.gross_total - x.detail_gross) > 0.005
    OR abs(x.net_total   - x.detail_net)   > 0.005;

-- ---------------------------------------------------------------------------
-- 8) A7：明细 ↔ 明细项对账（Σ EARNING − Σ PRE_DEDUCT = gross；Σ WITHHOLD = withhold）
--    SUMMARY 项（GROSS/NET）与 EMPLOYER 项（SI_COMPANY/HF_COMPANY）不参与这两个和
-- ---------------------------------------------------------------------------
INSERT INTO hr_inv_result
SELECT 'A7', 'BLOCKER',
       '明细汇总列必须等于明细项按类型求和',
       count(*),
       left(coalesce(min(x.employee_no
                          || '：gross ' || x.gross_amount || ' vs 项 ' || x.item_gross
                          || ' / withhold ' || x.withhold_amount || ' vs 项 ' || x.item_withhold), ''), 200)
  FROM (SELECT d.id, d.employee_no, d.gross_amount, d.withhold_amount,
               coalesce(sum(i.amount) FILTER (WHERE i.item_type = 'EARNING'), 0)
             - coalesce(sum(i.amount) FILTER (WHERE i.item_type = 'PRE_DEDUCT'), 0) AS item_gross,
               coalesce(sum(i.amount) FILTER (WHERE i.item_type = 'WITHHOLD'), 0) AS item_withhold
          FROM biz_salary_detail d
          JOIN biz_salary_sheet s ON s.id = d.sheet_id AND s.deleted_at IS NULL
          LEFT JOIN biz_salary_detail_item i ON i.detail_id = d.id
         GROUP BY d.id, d.employee_no, d.gross_amount, d.withhold_amount) x
 WHERE abs(x.gross_amount    - x.item_gross)    > 0.005
    OR abs(x.withhold_amount - x.item_withhold) > 0.005;

-- ---------------------------------------------------------------------------
-- 9) A8：已过账/已红冲的薪资单必须有那两张凭证，且借贷平衡、来源类型对
-- ---------------------------------------------------------------------------
INSERT INTO hr_inv_result
SELECT 'A8', 'BLOCKER',
       'POSTED/REVERSED 单据必须有计提+发放两张平衡凭证',
       count(*),
       left(coalesce(min(s.sheet_no || '：' ||
            CASE WHEN s.accrual_voucher_id IS NULL THEN '缺计提凭证 '
                 WHEN va.id IS NULL               THEN '计提凭证不存在 '
                 WHEN va.source_type <> 'PAYROLL_ACCRUAL' THEN '计提 source_type=' || va.source_type || ' '
                 WHEN va.total_debit <> va.total_credit   THEN '计提不平 '
                 ELSE '' END ||
            CASE WHEN s.payment_voucher_id IS NULL THEN '缺发放凭证 '
                 WHEN vp.id IS NULL               THEN '发放凭证不存在 '
                 WHEN vp.source_type <> 'PAYROLL_PAYMENT' THEN '发放 source_type=' || vp.source_type || ' '
                 WHEN vp.total_debit <> vp.total_credit   THEN '发放不平'
                 ELSE '' END), ''), 200)
  FROM biz_salary_sheet s
  LEFT JOIN biz_voucher va ON va.id = s.accrual_voucher_id AND va.deleted_at IS NULL
  LEFT JOIN biz_voucher vp ON vp.id = s.payment_voucher_id AND vp.deleted_at IS NULL
 WHERE s.deleted_at IS NULL
   AND s.status IN ('POSTED', 'REVERSED')
   AND (s.accrual_voucher_id IS NULL
     OR s.payment_voucher_id IS NULL
     OR va.id IS NULL
     OR vp.id IS NULL
     OR va.source_type <> 'PAYROLL_ACCRUAL'
     OR vp.source_type <> 'PAYROLL_PAYMENT'
     OR va.total_debit <> va.total_credit
     OR vp.total_debit <> vp.total_credit);

-- ---------------------------------------------------------------------------
-- 10) A9：凭证期间必须存在 + 已结账期间里不许有会永久过不了账的死单
--     （为什么不是设计字面的「period status <> CLOSED」，见文件头「两处偏差」①）
-- ---------------------------------------------------------------------------
INSERT INTO hr_inv_result
SELECT 'A9', 'BLOCKER',
       '已过账单据的凭证期间必须存在，且已结账期间不许留待过账死单',
       count(*),
       left(coalesce(min(x.tag), ''), 200)
  FROM (
        -- a) 凭证挂在黑期间（biz_accounting_period 里没有这个 period_code）
        SELECT '凭证期间不存在 ' || s.sheet_no || '/' || v.period AS tag
          FROM biz_salary_sheet s
          JOIN biz_voucher v
            ON v.id IN (s.accrual_voucher_id, s.payment_voucher_id)
           AND v.deleted_at IS NULL
          LEFT JOIN biz_accounting_period p ON p.period_code = v.period
         WHERE s.deleted_at IS NULL
           AND s.status IN ('POSTED', 'REVERSED')
           AND p.id IS NULL
        UNION ALL
        -- b) 已结账期间里躺着审批中/已批准的单 ⇒ 过账的期间闸会永久拒绝 ⇒ 死单
        SELECT '已结账期间仍有待过账单据 ' || s.sheet_no || '/' || s.status || '/' || s.period_code AS tag
          FROM biz_salary_sheet s
          JOIN biz_accounting_period p
            ON p.period_code = s.period_code AND p.status = 'CLOSED'
         WHERE s.deleted_at IS NULL
           AND s.status IN ('SUBMITTED', 'APPROVED')
       ) x;

-- ---------------------------------------------------------------------------
-- 11) A10：请假单 ↔ 审批实例
--     SUBMITTED ⇒ 实例在跑（PENDING）；APPROVED ⇒ 实例已通过（含免审直过，见文件头偏差②）
-- ---------------------------------------------------------------------------
INSERT INTO hr_inv_result
SELECT 'A10', 'BLOCKER',
       '请假单状态必须与审批实例状态一致',
       count(*),
       left(coalesce(min(l.leave_no || '/' || l.status || '/' ||
            CASE WHEN l.approval_instance_id IS NULL THEN '无实例'
                 WHEN i.id IS NULL                  THEN '实例不存在/已删'
                 ELSE '实例 ' || i.status END), ''), 200)
  FROM biz_leave_request l
  LEFT JOIN biz_flow_instance i ON i.id = l.approval_instance_id AND i.deleted_at IS NULL
 WHERE l.deleted_at IS NULL
   AND (   (l.status = 'SUBMITTED'
            AND (l.approval_instance_id IS NULL OR i.id IS NULL OR i.status <> 'PENDING'))
        OR (l.status = 'APPROVED'
            AND (l.approval_instance_id IS NULL OR i.id IS NULL OR i.status <> 'APPROVED')));

-- ---------------------------------------------------------------------------
-- 12) W1（WARN）：考勤天数守恒（口径因客户而异，只报不拦）
-- ---------------------------------------------------------------------------
INSERT INTO hr_inv_result
SELECT 'W1', 'WARN',
       '考勤守恒：出勤+各类假+旷工 ≈ 应出勤（±0.01）',
       count(*),
       left(coalesce(min(r.employee_no || '：应' || r.should_days || ' 实+假+旷=' ||
            (r.actual_days + r.leave_paid_days + r.leave_unpaid_days + r.sick_days + r.absent_days)), ''), 200)
  FROM biz_attendance_record r
 WHERE r.deleted_at IS NULL
   AND r.should_days > 0
   AND abs(r.actual_days + r.leave_paid_days + r.leave_unpaid_days
           + r.sick_days + r.absent_days - r.should_days) > 0.01;

-- ---------------------------------------------------------------------------
-- 13) W2（WARN）：账号部门 ↔ 员工人事部门漂移（借调合法，只报不拦）
-- ---------------------------------------------------------------------------
INSERT INTO hr_inv_result
SELECT 'W2', 'WARN',
       '账号部门与员工人事部门漂移（借调合法，仅观察）',
       count(*),
       left(coalesce(min(e.emp_no || '：人事 ' || e.dept_id || ' / 账号 ' || u.dept_id), ''), 200)
  FROM biz_employee e
  JOIN sys_user u ON u.id = e.user_id AND u.deleted_at IS NULL
 WHERE e.deleted_at IS NULL
   AND e.user_id IS NOT NULL
   AND u.dept_id IS NOT NULL
   AND u.dept_id <> e.dept_id;

-- ---------------------------------------------------------------------------
-- 14) 打印结果（每行一条，机器可读：`A1 BLOCKER viol=0 …`；人读：后面跟说明）
--     unaligned：不加前后空白填充，脚本用 `^A1 ` 就能精确锚定，不用去容错空格
-- ---------------------------------------------------------------------------
\pset format unaligned
SELECT format('%-3s %-7s viol=%-4s %s%s',
              no, level, viol, name,
              CASE WHEN viol > 0 AND sample IS NOT NULL AND sample <> ''
                   THEN ' ‹例：' || sample || '›' ELSE '' END) AS "HR 巡检"
  FROM hr_inv_result
 ORDER BY no;

-- ---------------------------------------------------------------------------
-- 15) 汇总行（脚本解析用，别改格式；改了两边一起改）
-- ---------------------------------------------------------------------------
SELECT format('HR_INVARIANTS_SUMMARY blocker_fail=%s warn_fail=%s total=%s',
              (SELECT count(*) FROM hr_inv_result WHERE level = 'BLOCKER' AND viol > 0),
              (SELECT count(*) FROM hr_inv_result WHERE level = 'WARN'    AND viol > 0),
              (SELECT count(*) FROM hr_inv_result))
  AS "HR 巡检汇总";

-- ---------------------------------------------------------------------------
-- 16) 拦：BLOCKER 有违规就 RAISE（裸跑 psql 时退出码非 0）
--     report_only=1（PGOPTIONS='-c hr.inv.report_only=1'）时只打印不拦，
--     供「造错自检」脚本在同一次事务里逐个注入并观察结果。
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_block      bigint;
    v_list       text;
    v_report_only boolean := coalesce(nullif(current_setting('hr.inv.report_only', true), ''), '0')
                            IN ('1', 'true', 'on', 'TRUE');
BEGIN
    SELECT count(*), string_agg(no || '(' || viol || ')', ' / ')
      INTO v_block, v_list
      FROM hr_inv_result
     WHERE level = 'BLOCKER' AND viol > 0;

    IF v_block > 0 THEN
        IF v_report_only THEN
            RAISE NOTICE 'HR 巡检：% 条 BLOCKER 违规（report_only=1，不中断）：%', v_block, v_list;
        ELSE
            RAISE EXCEPTION 'HR 巡检失败：% 条 BLOCKER 断言有违规 → %（明细见上表）',
                v_block, v_list
                USING HINT = '按断言编号查脚本头注释的语义；A8 多半是过账后凭证被删/被改，'
                             'A6/A7 是明细与汇总不同步（先查有没有绕过引擎的写路径）。';
        END IF;
    ELSE
        RAISE NOTICE 'HR 巡检通过：8 条 BLOCKER 断言零违规（A4/A5/W1/W2 为 WARN 观察项）';
    END IF;
END $$;
