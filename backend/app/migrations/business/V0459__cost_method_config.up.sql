-- ============================================================================
-- V0459：成本方法配置（M5-01【M5·成本】品种法 / 分批法 / 标准成本）
--
--   ① biz_cost_method_config  成本核算方法配置（一行 = 一个范围在一段生效期里用哪种方法）
--   ② 一条 GLOBAL/JOB 种子行：一期默认口径（ARCH-9 D-3「一期只做分批法」）显式落库，
--      页面一打开就能看见「全公司默认 = 分批法」，而不是空白页 + 靠代码兜底
--   ③ 索引（同范围同生效日唯一 / 对象扫 / 生效期扫）+ 末尾 DO 块自检
--
-- ── 号段核对（动手前 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail`）
--   写入本文件时全局最大号 = V0458（V0458__erp_contract_execution），V0459 空闲。
--   表名 / 约束前缀 `cmc`（cost method config）在全库迁移里 grep 无占用；
--   菜单 UUID 段 …d101 / …d111–d114 / 种子行 …d001 在 `sys_menu` / 本表里均未占用。
--
-- ── 卡面逐条落地（「按成本对象选方法」，本卡只做这一件事）──────────────────
--   1) 三种方法 ⇒ 一列 `method`，值域 **JOB 分批法 / VARIETY 品种法 / STANDARD 标准成本**。
--      为什么不复用 `biz_material.costing_method`：那个列是**存货计价法**
--      （MOVING_AVG / FIFO / STANDARD），V0201 已按 ARCH-9 D-2 收敛为只允许 MOVING_AVG，
--      代码 `erp/dto.rs` 同口径。成本**核算方法**（按什么粒度归集料工费）是另一个维度：
--      分批法按生产批、品种法按产品、标准成本按标准卡 + 差异，与「出库按什么价结转」
--      互不影响（ARCH-9 §2 已把两件事分开写）。复活那个列的值域 = 每条出库路径都要改，
--      价值只体现在「下拉框里多两个选项」，D-2 明确不做。
--
--   2) 「按成本对象选方法」⇒ `scope_type` 两级 + `scope_id`：
--      · `GLOBAL`      —— 全公司默认（`scope_id` 必须为空）；
--      · `COST_OBJECT` —— 指定成本对象（`scope_id` 必填，弱引用 `biz_cost_object.id`）。
--      判定顺序（`cost_method_dto::resolve`，纯函数）：**对象级 > 全局级 > 代码默认 JOB**。
--      为什么不做「按产品 / 按车间」两级：ARCH-9 §7 的切换卡（M5-31）还没定这类粒度，
--      现在多造的层级没有写入方，就是死结构（V0450 头注释 6 同一条理由）。
--      真要加，追加迁移放行新的 `scope_type` 值即可，不动本表结构。
--
--   3) 生效期 `effective_from` / `effective_to`（止可空 = 长期有效）：
--      方法切换（M5-31）要能回答「2026-11 这期按什么算」，只有一列 method 是答不出来的
--      —— 改一次就把历史口径改写了。所以按「生效日版本 + 停止日」表达：
--      切换 = 把旧版 `effective_to` 收到切换日前一天 + 新建一条 `effective_from = 切换日` 的版本。
--      同范围同生效日只允许一条（唯一索引），区间重叠由 handler 拦（409 + 66102）。
--
--   4) `status`（1 启用 / 0 停用）：停用不影响任何已算期间（它只是「以后别再用这条」），
--      与「删除」分开 —— 见 5)。
--
--   5) **删除纪律**：已生效过（`effective_from <= 今天`）的配置**不许删**，只能停用。
--      理由：配置行本身是「这段期间按哪种方法算的」的凭据，删掉就说不清当期口径
--      （AGENTS 约定 11 的「死单」形态：事后没人能解释这笔账怎么算的）。
--      handler 对这类删除回 409 + 66104，报文引导「改成停用 / 改生效日到未来」。
--
--   6) 一期引擎只实现分批法（ARCH-9 D-3）：`method <> 'JOB'` 的配置**允许维护**
--      （它是二期 / 三期的排期数据，也是 M5-31 切换策略的输入），但成本计算入口
--      （`cost_calc_period`）会**拒绝**按未实现的方法计算并报 409 + 66101，
--      绝不静默按分批法算完却报成标准成本 —— 那样出来的报表是错的，且没人看得出来。
--
--   7) 数据权限：本表与分摊规则 / 费率同口径 **SCOPE-EXEMPT** —— 成本方法配置是
--      全公司财务口径（不是某部门的业务单据），本卡不登记 `shared::scope::SCOPED_TABLES`
--      （本表也没有 dept 归属列），可见性交给权限码。
--
-- ── 幂等与回滚 ───────────────────────────────────────────────────────────
--   CREATE TABLE / INDEX IF NOT EXISTS；种子行按**主键**判重（`WHERE NOT EXISTS … WHERE id = …d001`，
--   所以用户软删过它之后重跑本迁移**不会**把它复活）。
--   末尾 DO 块自检：表 / 列数 / 四条 CHECK 各自拒坏数据 / 唯一索引拒同日重复 /
--   种子行形态 / 探针清理。
--   回滚见 V0459__cost_method_config.down.sql：**有数据就拒绝回滚**（配置是口径凭据），
--   再删表。
-- ============================================================================

-- ---------- ① biz_cost_method_config（成本核算方法配置） ----------
CREATE TABLE IF NOT EXISTS biz_cost_method_config (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    method         VARCHAR(16)   NOT NULL,                          -- 核算方法：JOB 分批法 / VARIETY 品种法 / STANDARD 标准成本
    scope_type     VARCHAR(16)   NOT NULL,                          -- 范围类型：GLOBAL 全公司默认 / COST_OBJECT 指定成本对象
    scope_id       UUID,                                            -- 成本对象（弱引用 biz_cost_object.id；GLOBAL 时必须为空）
    effective_from DATE          NOT NULL,                          -- 生效日（含）
    effective_to   DATE,                                            -- 停止日（含）；NULL = 长期有效
    status         SMALLINT      NOT NULL DEFAULT 1,                -- 1 启用 / 0 停用（停用不影响历史期间）
    remark         TEXT,                                            -- 备注（切换原因 / 审批单号）
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,
    deleted_at     TIMESTAMPTZ,                                     -- 软删时间（NULL = 存活）

    CONSTRAINT ck_biz_cmc_method
        CHECK (method IN ('JOB','VARIETY','STANDARD')),
    CONSTRAINT ck_biz_cmc_scope
        CHECK (scope_type IN ('GLOBAL','COST_OBJECT')),
    -- 范围与对象必须成对：全局没有对象；对象级必须指名对象（半套参数 = 口径不明）
    CONSTRAINT ck_biz_cmc_scope_pair
        CHECK ((scope_type = 'GLOBAL'      AND scope_id IS NULL)
            OR (scope_type = 'COST_OBJECT' AND scope_id IS NOT NULL)),
    CONSTRAINT ck_biz_cmc_range
        CHECK (effective_to IS NULL OR effective_to >= effective_from),
    CONSTRAINT ck_biz_cmc_status
        CHECK (status IN (0,1))
);

COMMENT ON TABLE  biz_cost_method_config IS '成本核算方法配置（M5-01）：谁（全公司 / 某个成本对象）从哪天到哪天用哪种核算方法（分批法 / 品种法 / 标准成本）；一期引擎只实现分批法，其余方法可配置但计算入口拒绝';
COMMENT ON COLUMN biz_cost_method_config.method IS '核算方法：JOB 分批法（一期唯一实现）/ VARIETY 品种法（三期）/ STANDARD 标准成本（二期只做差异分析）；与 biz_material.costing_method（存货计价法）不是一件事';
COMMENT ON COLUMN biz_cost_method_config.scope_type IS '范围：GLOBAL 全公司默认 / COST_OBJECT 指定成本对象；判定顺序 对象级 > 全局级 > 代码默认 JOB';
COMMENT ON COLUMN biz_cost_method_config.scope_id IS '成本对象（弱引用 biz_cost_object.id，不建物理外键）；scope_type=GLOBAL 时必须为空（ck_biz_cmc_scope_pair）';
COMMENT ON COLUMN biz_cost_method_config.effective_from IS '生效日（含）：期间内任一天起生效的非分批法配置都会让该期成本计算被拒（一期引擎限制）';
COMMENT ON COLUMN biz_cost_method_config.effective_to IS '停止日（含）；NULL = 长期有效。切换方法 = 旧版收到切换日前一天 + 新建切换日生效的版本';
COMMENT ON COLUMN biz_cost_method_config.status IS '1 启用 / 0 停用；停用不影响已算期间（历史口径靠 effective_from/effective_to 表达）';

-- 同一范围同一生效日只有一条版本（软删后可重排同一天，与其它单据同口径）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cmc_global_effective
    ON biz_cost_method_config (effective_from)
 WHERE deleted_at IS NULL AND scope_type = 'GLOBAL';
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cmc_object_effective
    ON biz_cost_method_config (scope_id, effective_from)
 WHERE deleted_at IS NULL AND scope_type = 'COST_OBJECT';
-- 按对象查（对象详情 / 计算入口逐对象判定）
CREATE INDEX IF NOT EXISTS idx_biz_cmc_object
    ON biz_cost_method_config (scope_id)
 WHERE deleted_at IS NULL AND scope_type = 'COST_OBJECT';
-- 按生效期扫（「这段时间用的什么方法」/ M5-31 切换策略回看）
CREATE INDEX IF NOT EXISTS idx_biz_cmc_effective
    ON biz_cost_method_config (effective_from DESC, method)
 WHERE deleted_at IS NULL AND status = 1;

-- ---------- ② 种子：一期默认口径（全公司 = 分批法） ----------
--   按主键判重：软删过之后重跑本迁移不会复活它（用户删了就是删了）。
INSERT INTO biz_cost_method_config
    (id, method, scope_type, scope_id, effective_from, effective_to, status, remark)
SELECT '20000000-0000-4000-8000-00000000d001'::uuid, 'JOB', 'GLOBAL', NULL, DATE '1970-01-01', NULL, 1,
       '一期默认口径：ARCH-9 D-3「一期只做分批法」，成本对象按生产批归集；改方法请新增一条生效日更晚的版本'
 WHERE NOT EXISTS (SELECT 1 FROM biz_cost_method_config
                    WHERE id = '20000000-0000-4000-8000-00000000d001'::uuid);

-- ---------- ③ 自检（任一不符 → 迁移失败） ----------
DO $$
DECLARE
    v_cols    integer;
    v_checks  integer;
    v_idx     integer;
    v_seed    integer;
    v_reject  integer := 0;
    probe     uuid := gen_random_uuid();
    probe_obj uuid := gen_random_uuid();
    probe2    uuid := gen_random_uuid();
BEGIN
    -- 3.1 表在 + 列数（13 列：8 业务 + 5 审计/时间/软删）
    SELECT count(*) INTO v_cols
      FROM information_schema.columns WHERE table_name = 'biz_cost_method_config';
    IF v_cols <> 13 THEN
        RAISE EXCEPTION 'V0459 自检失败：biz_cost_method_config 应有 13 列，实际 %', v_cols;
    END IF;

    -- 3.2 五条 CHECK 都在（方法 / 范围 / 成对 / 区间 / 状态）
    SELECT count(*) INTO v_checks
      FROM pg_constraint
     WHERE conrelid = 'biz_cost_method_config'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cmc_method','ck_biz_cmc_scope','ck_biz_cmc_scope_pair',
                       'ck_biz_cmc_range','ck_biz_cmc_status');
    IF v_checks <> 5 THEN
        RAISE EXCEPTION 'V0459 自检失败：CHECK 约束应有 5 条，实际 %', v_checks;
    END IF;

    -- 3.3 索引 4 条（2 唯一 + 2 普通）
    SELECT count(*) INTO v_idx
      FROM pg_indexes
     WHERE tablename = 'biz_cost_method_config'
       AND indexname IN ('uk_biz_cmc_global_effective','uk_biz_cmc_object_effective',
                         'idx_biz_cmc_object','idx_biz_cmc_effective');
    IF v_idx <> 4 THEN
        RAISE EXCEPTION 'V0459 自检失败：索引应有 4 条，实际 %', v_idx;
    END IF;

    -- 3.4 种子行形态（GLOBAL / 无对象 / JOB / 启用）
    SELECT count(*) INTO v_seed
      FROM biz_cost_method_config
     WHERE id = '20000000-0000-4000-8000-00000000d001'::uuid
       AND method = 'JOB' AND scope_type = 'GLOBAL' AND scope_id IS NULL AND status = 1;
    IF v_seed <> 1 THEN
        RAISE EXCEPTION 'V0459 自检失败：默认口径种子行（GLOBAL/JOB）缺失或被改';
    END IF;

    -- 3.5 四条 CHECK 逐条拒坏数据（探针，验完即删）
    BEGIN
        INSERT INTO biz_cost_method_config (id, method, scope_type, effective_from)
        VALUES (probe, 'FIFO', 'GLOBAL', DATE '2026-01-01');
        v_reject := v_reject + 1;
    EXCEPTION WHEN check_violation THEN NULL; END;

    BEGIN
        INSERT INTO biz_cost_method_config (id, method, scope_type, scope_id, effective_from)
        VALUES (probe, 'JOB', 'GLOBAL', probe_obj, DATE '2026-01-01');
        v_reject := v_reject + 1;   -- 全局却带对象
    EXCEPTION WHEN check_violation THEN NULL; END;

    BEGIN
        INSERT INTO biz_cost_method_config (id, method, scope_type, effective_from)
        VALUES (probe, 'JOB', 'COST_OBJECT', DATE '2026-01-01');
        v_reject := v_reject + 1;   -- 对象级却没对象
    EXCEPTION WHEN check_violation THEN NULL; END;

    BEGIN
        INSERT INTO biz_cost_method_config (id, method, scope_type, effective_from, effective_to)
        VALUES (probe, 'JOB', 'GLOBAL', DATE '2026-02-01', DATE '2026-01-01');
        v_reject := v_reject + 1;   -- 停止日早于生效日
    EXCEPTION WHEN check_violation THEN NULL; END;

    IF v_reject <> 0 THEN
        RAISE EXCEPTION 'V0459 自检失败：% 条坏数据被放行（CHECK 没生效）', v_reject;
    END IF;

    -- 3.6 唯一索引真拒「同对象同生效日」
    INSERT INTO biz_cost_method_config (id, method, scope_type, scope_id, effective_from, status)
    VALUES (probe, 'JOB', 'COST_OBJECT', probe_obj, DATE '2026-05-01', 1);
    BEGIN
        INSERT INTO biz_cost_method_config (id, method, scope_type, scope_id, effective_from, status)
        VALUES (probe2, 'VARIETY', 'COST_OBJECT', probe_obj, DATE '2026-05-01', 1);
        v_reject := v_reject + 1;
    EXCEPTION WHEN unique_violation THEN NULL; END;
    IF v_reject <> 0 THEN
        RAISE EXCEPTION 'V0459 自检失败：同对象同生效日被放行（uk_biz_cmc_object_effective 没生效）';
    END IF;

    -- 3.7 探针清理（重跑零残留）
    DELETE FROM biz_cost_method_config WHERE id IN (probe, probe2);

    RAISE NOTICE 'V0459 完成：biz_cost_method_config 就绪（5 CHECK / 4 索引 / 默认 GLOBAL=JOB 种子 1 行）';
END $$;
