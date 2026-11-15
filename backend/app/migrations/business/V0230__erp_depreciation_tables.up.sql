-- ======================================================
-- V0230：折旧批次 + 折旧明细 + 3 个权限码 + 折旧计提菜单 + 定时任务行
--       （ARCH-6 §5.2.4 / §5.2.5 / §5.5 / §6.8）
--
-- 表（DDL 逐字取自 ARCH-6 §5.2）：
--   · biz_depreciation_batch  一次计提 = 一行（**一个期间一批**）
--   · biz_depreciation        一台资产一个期间一行
--
-- ── 本迁移最要紧的两条约束 ────────────────────────────────
-- ① `uk_biz_dep_batch_period`（`period_code` 全表唯一）＝ 定时任务天然幂等：
--    重跑撞这条键，不会提两次（ARCH-6 §5.2.4）。
--    ⚠️ 因此**「期间已结账」的拒绝不落批次行** —— 落了 SKIPPED 行之后，
--    反结账再跑就会永久撞 `uk_biz_dep_batch_period`（详见 §5.5.5 的取舍说明，
--    实施差异也写进证据 README）。
-- ② `uk_biz_dep_asset_period`（`asset_id, period_code` 唯一）＝ 同一资产同月不许提两次，
--    即便有人绕过批次直接写明细，也提不出第二行。
--
-- ── 权限码（3 个）───────────────────────────────────────
--   depreciation: list / run / reverse
--
-- ── 菜单 ────────────────────────────────────────────────
--   折旧计提（1403）挂在 V0229 建的「固定资产」目录（1400）下；
--   按钮码 1431 手动计提 / 1432 红冲。
--
-- ── 定时任务（ARCH-6 §6.8）──────────────────────────────
--   `erp_depreciation_run`，cron `0 30 1 1 * *`（每月 1 日 01:30 计提**上一个**会计月）。
--   sys_job 这一行 `status = 0`（停用）：真正的调度由进程内 `register_cron` 注册
--   （与 4 条提醒任务同款，见 `task/mod.rs`），本行只供运维在「定时任务」页查看 /
--   手动触发。写 `status = 1` 会变成两套调度同时跑（幂等能挡住错误，但日志会重复）。
--
-- ── 幂等与回滚 ───────────────────────────────────────────
-- 建表 / 索引 `IF NOT EXISTS`；菜单 `ON CONFLICT (id) DO NOTHING`；sys_job `NOT EXISTS`。
-- ======================================================

-- ---------- ① biz_depreciation_batch（折旧批次） ----------
CREATE TABLE IF NOT EXISTS biz_depreciation_batch (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    batch_no     VARCHAR(32)   NOT NULL,
    period_code  VARCHAR(7)    NOT NULL,
    status       VARCHAR(16)   NOT NULL DEFAULT 'CALCULATED',
    asset_count  INT           NOT NULL DEFAULT 0,
    total_amount NUMERIC(18,4) NOT NULL DEFAULT 0,
    voucher_id   UUID,
    reverse_voucher_id UUID,
    skip_reason  VARCHAR(255),
    run_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    run_by       UUID,
    posted_at    TIMESTAMPTZ,
    reversed_at  TIMESTAMPTZ,
    remark       TEXT,
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_biz_dep_batch_status CHECK (status IN
        ('CALCULATED','POSTED','REVERSED','SKIPPED')),
    CONSTRAINT ck_biz_dep_batch_amount CHECK (total_amount >= 0 AND asset_count >= 0),
    CONSTRAINT ck_biz_dep_batch_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    -- 已过账必须有凭证；跳过必须写原因
    CONSTRAINT ck_biz_dep_batch_posted CHECK (status <> 'POSTED' OR voucher_id IS NOT NULL),
    CONSTRAINT ck_biz_dep_batch_skip   CHECK (status <> 'SKIPPED' OR skip_reason IS NOT NULL),
    -- 已红冲必须有两个凭证指针
    CONSTRAINT ck_biz_dep_batch_rev    CHECK (
        status <> 'REVERSED' OR (voucher_id IS NOT NULL AND reverse_voucher_id IS NOT NULL))
);

-- ★ 一个期间只能有一批（幂等的库级保证：定时任务重跑 / 人工重跑都撞这条）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_dep_batch_period ON biz_depreciation_batch (period_code);
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_dep_batch_no     ON biz_depreciation_batch (batch_no);
CREATE INDEX IF NOT EXISTS idx_biz_dep_batch_status ON biz_depreciation_batch (status, period_code DESC);
CREATE INDEX IF NOT EXISTS idx_biz_dep_batch_voucher ON biz_depreciation_batch (voucher_id)
    WHERE voucher_id IS NOT NULL;

COMMENT ON TABLE biz_depreciation_batch IS
    '折旧批次（一个期间一批；period_code 全表唯一 = 定时任务天然幂等）';

-- ---------- ② biz_depreciation（折旧明细） ----------
CREATE TABLE IF NOT EXISTS biz_depreciation (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    batch_id                UUID          NOT NULL REFERENCES biz_depreciation_batch(id) ON DELETE CASCADE,
    asset_id                UUID          NOT NULL REFERENCES biz_fixed_asset(id),
    asset_no                VARCHAR(32)   NOT NULL,
    asset_name              VARCHAR(200)  NOT NULL,
    period_code             VARCHAR(7)    NOT NULL,
    dept_id                 UUID          NOT NULL,
    dept_name               VARCHAR(120)  NOT NULL,
    expense_account_id      UUID          NOT NULL,
    expense_account_code    VARCHAR(30)   NOT NULL,
    amount                  NUMERIC(18,4) NOT NULL,
    accumulated_before      NUMERIC(18,4) NOT NULL,
    accumulated_after       NUMERIC(18,4) NOT NULL,
    net_value_after         NUMERIC(18,4) NOT NULL,
    is_last_month           SMALLINT      NOT NULL DEFAULT 0,
    status                  VARCHAR(12)   NOT NULL DEFAULT 'POSTED',
    reversed_at             TIMESTAMPTZ,
    remark                  VARCHAR(255),
    created_at              TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_dep_asset_period UNIQUE (asset_id, period_code),
    CONSTRAINT ck_biz_dep_amount   CHECK (amount >= 0),
    CONSTRAINT ck_biz_dep_accum    CHECK (accumulated_before >= 0
                                          AND accumulated_after = accumulated_before + amount),
    CONSTRAINT ck_biz_dep_net      CHECK (net_value_after >= 0),
    CONSTRAINT ck_biz_dep_last     CHECK (is_last_month IN (0,1)),
    CONSTRAINT ck_biz_dep_status   CHECK (status IN ('POSTED','REVERSED')),
    CONSTRAINT ck_biz_dep_period   CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$')
);

CREATE INDEX IF NOT EXISTS idx_biz_dep_batch  ON biz_depreciation (batch_id);
CREATE INDEX IF NOT EXISTS idx_biz_dep_asset  ON biz_depreciation (asset_id, period_code DESC);
CREATE INDEX IF NOT EXISTS idx_biz_dep_period ON biz_depreciation (period_code, status);
CREATE INDEX IF NOT EXISTS idx_biz_dep_dept   ON biz_depreciation (dept_id, period_code);

COMMENT ON TABLE biz_depreciation IS
    '折旧明细（一台资产一个期间一行；uk(asset_id, period_code) 防止同一资产同月重复计提）';

-- ---------- ③ 权限码与菜单 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001403'::uuid, '折旧计提', 2, 'business:erp:depreciation:list',
     '/business/erp/depreciation', '20000000-0000-0000-0000-000000001400'::uuid,
     'business/erp/depreciation/index', 30, 1, 'chart-line', '折旧计提（月度批次 / 凭证 / 红冲）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001431'::uuid, 'business:erp:depreciation:run', 3, 'business:erp:depreciation:run',
     NULL, '20000000-0000-0000-0000-000000001403'::uuid, NULL, 10, 1, NULL, '手动计提折旧（目标期间 = 上一会计月）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001432'::uuid, 'business:erp:depreciation:reverse', 3, 'business:erp:depreciation:reverse',
     NULL, '20000000-0000-0000-0000-000000001403'::uuid, NULL, 20, 1, NULL, '红冲折旧批次（红字凭证 + 回滚卡片）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001403'::uuid),
        ('20000000-0000-0000-0000-000000001431'::uuid),
        ('20000000-0000-0000-0000-000000001432'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'FINANCE_MANAGER', 'FINANCE_DIRECTOR', 'DEPT_LEADER')
   AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id);

-- ---------- ④ 定时任务行（仅供查看 / 手动触发；真实调度在 task/mod.rs） ----------
INSERT INTO sys_job (job_name, job_group, cron_expr, invoke_target, params, status, remark)
SELECT v.job_name, 'ERP', v.cron_expr, v.invoke_target, NULL, 0, v.remark
  FROM (VALUES
        ('erp_depreciation_run', '0 30 1 1 * *', 'erp_depreciation_run',
         '折旧计提（内置任务已注册，本行仅供查看/手动执行）：每月 1 日 01:30 计提【上一个会计月】的折旧，按（费用科目, 部门）聚合生成一张多借方凭证并回写卡片')
       ) AS v(job_name, cron_expr, invoke_target, remark)
 WHERE NOT EXISTS (SELECT 1 FROM sys_job j WHERE j.job_name = v.job_name AND j.deleted_at IS NULL);

-- ---------- ⑤ 自检 ----------
DO $$
DECLARE
    n int;
BEGIN
    IF to_regclass('public.biz_depreciation_batch') IS NULL
       OR to_regclass('public.biz_depreciation') IS NULL THEN
        RAISE EXCEPTION 'V0230 自检失败：折旧两表未全部建成';
    END IF;

    -- 幂等的两条库级保证
    IF NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'uk_biz_dep_batch_period') THEN
        RAISE EXCEPTION 'V0230 自检失败：uk_biz_dep_batch_period 缺失（一个期间一批的幂等保证没了）';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'uk_biz_dep_asset_period') THEN
        RAISE EXCEPTION 'V0230 自检失败：uk_biz_dep_asset_period 缺失（同资产同月可重复计提）';
    END IF;

    -- 明细 → 批次是 ON DELETE CASCADE，明细 → 资产是 RESTRICT（删卡前必须先想清楚）
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'biz_depreciation_asset_id_fkey') THEN
        RAISE EXCEPTION 'V0230 自检失败：biz_depreciation.asset_id 的外键缺失';
    END IF;

    -- 3 个权限码
    SELECT count(*) INTO n FROM sys_menu
     WHERE auth_code IN ('business:erp:depreciation:list','business:erp:depreciation:run','business:erp:depreciation:reverse')
       AND deleted_at IS NULL;
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0230 自检失败：折旧权限码种了 % 个（期望 3）', n;
    END IF;

    -- 页面菜单 + 定时任务行
    SELECT count(*) INTO n FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001403'::uuid AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0230 自检失败：折旧计提菜单未种';
    END IF;
    SELECT count(*) INTO n FROM sys_job
     WHERE job_name = 'erp_depreciation_run' AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0230 自检失败：erp_depreciation_run 任务行未种';
    END IF;
END $$;
