-- ============================================================================
-- V0366：种子数据补漏 —— 两条种子流程补 `is_system` + 全新库根部门（E2E-14 评审 H6/H5/N2）
--
-- 卡：t_810a3b70（REVIEW-FIX-2【I-3】种子补 is_system）
-- 上游：docs/review/20261109-系统全面评审.md（干净库 80 项不变量里唯一的真实 BLOCKER）
--
-- # 为什么是 V0366（不取 0363 / 0364）
--
--   迁移版本号**跨目录全局唯一**（多目录 loader：版本重复直接启动失败）。
--   当前最大是 `V0365__biz_status_log_action`，0363 / 0364 是并发在途卡预留的空位，
--   跳号不影响执行顺序（按数值升序），所以本卡取 0366。
--
-- # 一、缺陷现场（干净库 cloudpivot_fresh 实测）
--
--   V0331（采购申请审批 `pr_approve`）/ V0332（采购退货审批 `prt_approve`）两条种子
--   INSERT 的列清单止于 `owner_dept_id`，没写 `is_system` ⇒ 吃 `DEFAULT FALSE`（V0193 定的）。
--   后果分两层，同一处漏列的两种表现：
--     · **H6（静态，BLOCKER）**：扫 migrations 的 `INSERT INTO biz_flow_def (…)` 列清单，
--       看不到 `is_system`，也没有后续迁移按 flow_code 补偿 ⇒ 红；
--       这是同一类错误第 5、6 次（V0209/V0211/V0228/V0231 之后）。
--     · **H5（库内，WARN）**：这两条的 flow_name 是「采购申请审批（标准·超预算加签）」
--       「采购退货审批（标准·两级）」—— 名字自称「标准」，`is_system` 却是 false。
--   业务后果不是报错，而是**静默少卡**：P2-6b 把模板库默认口径收口成
--   `AND d.is_system` 之后，这两条预置流程在「标准流程」列表里根本不出现；
--   同时以「客户自建」身份露出，客户可以删图（本该只能另存副本）。
--
-- # 二、修法：按**种子固定 UUID** 打标，flow_code 只作守卫
--
--   V0193 已立下口径：种子的 INSERT 条件是 `WHERE NOT EXISTS (flow_code = …)`，
--   客户若先建了同编码流程，种子整条跳过，那一行是**客户自己建的**；按 flow_code
--   打标会把它错认成系统预置（前端给客户的流程挂「标准」标签，设计意图反了）。
--   所以选择键是种子固定 UUID：
--       pr_approve  = 20000000-0000-4000-8000-0000000d0331（V0331:242）
--       prt_approve = 20000000-0000-4000-8000-0000000e0331（V0332:272）
--   同时带 `flow_code IN (…)` 作**双向守卫**（id 与编码都对上才动），顺带满足 H6 的
--   静态补偿判据（「更晚的迁移里有 UPDATE biz_flow_def + 该 flow_code 字面量 + is_system = TRUE」）。
--
-- # 三、顺带修 N2（全新库 `admin` 无部门，WARN）—— 迁移只做一半
--
--   N2 的判据是「在用 HUMAN 账号必须有部门」。干净库上它红的根因不是 admin 特殊，
--   而是**全新库根本没有组织树**（`sys_dept` 0 行）—— 部门由运维按环境用接口建
--   （`docs/architecture/P0-FIX-2B-…` D-1、`scripts/datafix/org_landing` 的口径：
--   「绝不猜、绝不造部门」）。所以这里只做**新装初始化**，不替客户编组织：
--     · 仅当组织树**完全为空**时，才种一个根部门「总部」（`dept_code='HQ'`，与
--       生产 / datafix 脚本用的编码一致）；已配好组织树的库（生产 4 个部门、
--       演练库的 drill 部门）**一行都不动**；
--     · `admin` 的落位**不在本迁移**：它由 `bootstrap::seed_super_admin` 在启动期建，
--       迁移执行时库里还没有这一行（实测 UPDATE 命中 0 行），所以落位在启动期做
--       （`system::dept::model::land_users_into_dept_by_code`，只认 `dept_code='HQ'`、
--       只补 `dept_id IS NULL`，每次启动幂等重试）。
--   超管本来就短路全部数据权限，落位不影响权限结果，图的是 N2 归零 + 新建单据能带部门。
--
-- # 四、幂等与回滚
--
--   ① 的 UPDATE 带 `IS DISTINCT FROM` 与固定的 id 集合：第二遍命中 0 行；
--   ② 的 INSERT 带 `WHERE NOT EXISTS (任意未删部门)`：第二遍插入 0 行。
--   回滚见 `V0366__seed_fix_flow_is_system_and_admin_dept.down.sql`
--   （只回退本迁移自己种的那一行 HQ：按固定 UUID 删，绝不误删运维建的部门）。
-- ============================================================================

-- ---------- ① 两条种子流程补 is_system（H6 / H5）----------
UPDATE biz_flow_def
   SET is_system = TRUE,
       updated_at = now()
 WHERE id IN (
       '20000000-0000-4000-8000-0000000d0331'::uuid, -- pr_approve  采购申请审批（V0331 种子）
       '20000000-0000-4000-8000-0000000e0331'::uuid  -- prt_approve 采购退货审批（V0332 种子）
       )
   AND flow_code IN ('pr_approve', 'prt_approve')
   AND deleted_at IS NULL
   AND is_system IS DISTINCT FROM TRUE;

-- 自检（H6 的库内落点）：种子 UUID 那两行只要还在，就必须 is_system = TRUE。
-- 若 V0331/V0332 当初因「客户已有同编码流程」整条跳过，这里 0 行，不算失败。
DO $$
DECLARE
    bad int;
BEGIN
    SELECT count(*) INTO bad
      FROM biz_flow_def
     WHERE id IN ('20000000-0000-4000-8000-0000000d0331'::uuid,
                  '20000000-0000-4000-8000-0000000e0331'::uuid)
       AND deleted_at IS NULL
       AND is_system IS DISTINCT FROM TRUE;
    IF bad > 0 THEN
        RAISE EXCEPTION 'V0366 自检失败：% 条种子流程（pr_approve / prt_approve）仍缺 is_system', bad;
    END IF;
END $$;

-- ---------- ② 全新库补一个根部门「总部」HQ（N2）----------
-- 只在「组织树完全为空」时种：已有部门的环境（生产 / 演练 / 客户真实组织）不动。
INSERT INTO sys_dept (id, dept_code, dept_name, parent_id, dept_path, leader_id,
                      status, sort_order, created_at, updated_at)
SELECT '20000000-0000-4000-8000-0000000f0366'::uuid,
       'HQ', '总部', NULL,
       '/20000000-0000-4000-8000-0000000f0366/', NULL,
       1, 1, now(), now()
 WHERE NOT EXISTS (SELECT 1 FROM sys_dept WHERE deleted_at IS NULL);

-- ---------- ③ 内置超管落位：**不在迁移里做**（见下）----------
--   `admin` 账号不是迁移建的，是 `bootstrap::seed_super_admin` 在**启动期**建的
--   （`ensure_admin_user`）—— 干净库实测：本迁移执行时 `sys_user` 里还没有 admin
--   这一行，"UPDATE sys_user … username = 'admin'" 命中 0 行，N2 照样红。
--   所以落位放在启动期做：`system::dept::model::land_users_into_dept_by_code`
--   （只认 `dept_code = HQ`、只补 `dept_id IS NULL`，启动时幂等重试）。
--   本迁移只负责 ②「全新库补一个根部门」，与「账号相关的事归 bootstrap」的分工一致。
