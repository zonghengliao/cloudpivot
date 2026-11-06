-- ======================================================
-- V0270：销售价格 —— 客户协议价 / 阶梯促销价 两张表 + 物料标准售价（SO-32，ARCH-7 §15.3.2 / §15.3.4）
--
--   ① biz_customer_price  客户协议价（优先级最高的一层取价来源）
--   ② biz_price_tier      阶梯价 / 促销价（客户专属 + 通用两用）
--   ③ ALTER biz_material  加 standard_price NUMERIC(18,4)（标准售价，取价链最后一层兜底）
--
-- 三张对象的取价优先级写在 ARCH-7 §15.3.3：
--   ① 客户协议价 → ② 客户专属阶梯 → ③ 通用阶梯 → ④ 物料标准售价 → ⑤ 空（前端手填）。
-- 本卡只落**存储**（表 / 约束 / 索引 / 一列），取价解析接口（GET …/price/resolve）
-- 与价格 CRUD 属后续卡；因此这里**不**新增任何 Rust 代码、不注册 DocKind / LEGAL_EDGES
-- （价格不是单据，没有状态机迁移边：status 只是「启用/停用」开关）。
--
-- ── 号段（开工实测复核）──────────────────────────────────
-- 卡面点名 `V0270__sales_price`。开工实测：backend/migrations/business/ 最大号 = V0306
-- （V0306__sales_return_menu_page，SO-31 提交 d2902f7），**V0270 未被任何迁移占用**
-- （V0268–V0279 是销售链的一整段空洞），故按卡面逐字取 V0270，不走「最大号 +1」。
-- 提交前再 `ls backend/migrations/business | sort | tail` 复核一次，防并行卡抢号。
--
--   乱序补迁在本仓是**允许**的：alun-db 的 `apply()` 对「版本号低于已应用最大值」的
--   versioned 迁移只打一条 `乱序补迁` 的 tracing::warn，不拒绝、不阻断（与 sqlx 同行为）。
--   本文件之所以敢用低号：它只新建两张表 + 给 biz_material 加一列，**不引用**任何
--   V0281+ 才出现的销售链对象（不碰 biz_sales_order / 发货 / 应收 / 发票 / 退货），
--   空库重放时天然排在 V0267 之后、V0280 之前，次序上也不需要谁先跑。
--
-- ── 字段口径（与卡面 / ARCH-7 §15.3.2 逐字对齐，别按别处注释改）──────
--  1) 金额列精度 `NUMERIC(18,4)`：ARCH-7 §15.3.2 对这两张表写的就是 (18,4)，与
--     biz_customer.credit_limit / biz_inventory.amount 同口径。注意**单据行**的
--     biz_sales_order_item.price 是 NUMERIC(18,6)（单据存更细的成交价，见 V0264），
--     取价时是 18,4 → 18,6 的**无损**扩位，方向不能反过来。自检块会断言本表 price
--     是 (18,4)，防止后人照单据口径改成 (18,6) —— 那会让「协议价 0.00005」这类
--     脏数据静默通过。
--  2) `min_qty` **NOT NULL DEFAULT 0**（设计稿只写「默认 0」，这里补上 NOT NULL，
--     原因见 3）。0 = 无起订量限制。
--  3) 唯一索引 uk_biz_cust_price 的四列 **(customer_id, material_id, min_qty, effective_from)**
--     全部 NOT NULL —— PG 的唯一索引里 NULL 彼此不相等，任何一列可空都会让
--     「同客户同物料同起订量同起期」插进第二条而不报错，价格从此有两条真相。
--     故：min_qty NOT NULL DEFAULT 0、effective_from NOT NULL DEFAULT CURRENT_DATE。
--     effective_from 给 DEFAULT 只是「今天起生效」这个最常见录入的便利，不是可空。
--  4) `price > 0`（卡面硬要求）：0 与负数一律 CHECK 拒绝。**不做**「价格为 0 表示免费」的
--     例外 —— 免费品走赠品/0 元行的是**单据**能力，不是价格表配置；价格表里出现 0
--     只会让取价链静默命中 0 元（比取不到价更危险，销售员与审批都看不出异常）。
--  5) `biz_material.standard_price` 可空（NULL = 未维护标准售价，与「维护了但为 0」语义
--     不同，取价链遇到 NULL 才继续往下走「无价可给」）；非空时必须 > 0，同一条纪律。
--
-- ── 外键口径（与 ADR-06 的差异，单列说明）────────────────────
--   ADR-06「单据链零外键」管的是**单据之间**的多态引用与锁死风险；本卡两张表是
--   **主数据配置**（不是单据），引用的 customer_id / material_id 都是单态引用，
--   且与 V0138 `biz_material_serial.material_id REFERENCES biz_material(id)`、
--   V0260 `biz_customer_contact.customer_id REFERENCES biz_customer(id) ON DELETE RESTRICT`
--   同风格 ⇒ 建**硬外键 ON DELETE RESTRICT**：
--     · 主数据走软删（deleted_at），正常业务永远不会撞外键；
--     · 物理删客户/物料时，若还挂着价格行会**被挡住**（RESTRICT），逼运维先处理价格 ——
--       比 CASCADE 静默吞掉价格配置安全（价格是「下一单按什么价开」的依据，静默消失
--       会让销售按裸价开单，事后无从追溯）。
--     · 已知代价：测试/导入清场脚本先删物料会被价格行挡住，必须**先删价格再删物料**
--       （仓库里已有大量 `DELETE FROM biz_material WHERE …` 的清场写法）。
--   customer_id 在本卡两张表里都不允许 NULL —— 「全客户通用」是用 biz_price_tier
--   的 `customer_id IS NULL` 表达的，biz_customer_price 本身是「某客户专属」的语义。
--
-- ── biz_price_tier.customer_id 可空 + 唯一性（NULLS NOT DISTINCT）────────
--   通用档位（customer_id IS NULL）必须能被唯一性管住，否则同一物料同一档位能录 N 条
--   互不相同的价格，取价规则（取 min_qty 最大）遇到并列就没有确定答案。
--   PG 默认「NULL 彼此不相等」，故唯一索引显式写 **NULLS NOT DISTINCT**（PG15+，
--   本机 18.6 实测；仓库既有先例 V0137 `uk_biz_inv_loc`）。ON CONFLICT 的索引推断
--   仍按列集合 + 谓词匹配，后续卡的 upsert 照写 `ON CONFLICT (material_id, customer_id,
--   tier_type, min_qty, effective_from) WHERE deleted_at IS NULL` 即可，不影响。
--   `effective_from` 在档位表也**NOT NULL DEFAULT CURRENT_DATE**：同上，可空列会让
--   唯一索引留下 NULL 缺口，且取价规则要按日期比较。
--
-- ── 区间语义（max_qty）───────────────────────────────
--   `min_qty <= 订购量 < max_qty`（左闭右开，ARCH-7 §15.3.3 ②）；max_qty NULL = 无上限。
--   因此 CHECK 是 `max_qty IS NULL OR max_qty > min_qty`（相等即空区间，无意义）。
--
-- ── 幂等 ──────────────────────────────────────────────
--   CREATE TABLE / INDEX IF NOT EXISTS；ADD COLUMN IF NOT EXISTS；ADD CONSTRAINT 先查
--   pg_constraint 再建。重复执行 no-op（自检块可反复跑）。
--   末尾 DO 块自检：表/列数/卡面列/金额列型/唯一索引语义/外键口径/CHECK 域值/默认值/
--   行为探针（坏数据必须被拒、唯一索引必须拦重、软删后可复用、RESTRICT 必须挡住物料删除）/
--   探针清场。
-- ======================================================

-- ---------- ① biz_customer_price（客户协议价，取价优先级最高） ----------
CREATE TABLE IF NOT EXISTS biz_customer_price (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),         -- 主键
    customer_id    UUID          NOT NULL,                             -- 客户（硬外键 → biz_customer.id）
    material_id    UUID          NOT NULL,                             -- 物料（硬外键 → biz_material.id）
    price          NUMERIC(18,4) NOT NULL,                             -- 含税协议单价（> 0）
    min_qty        NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 起订量（0 = 不限制；唯一键成员，必须 NOT NULL）
    effective_from DATE          NOT NULL DEFAULT CURRENT_DATE,         -- 生效日（含；唯一键成员，必须 NOT NULL）
    effective_to   DATE,                                                -- 失效日（含；NULL = 长期有效）
    status         VARCHAR(16)   NOT NULL DEFAULT 'ACTIVE',             -- 状态：ACTIVE 启用 / INACTIVE 停用
    remark         TEXT,                                                -- 备注
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,    -- 审计：创建时间
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,    -- 审计：更新时间（model 层维护）
    create_by      UUID,                                                -- 审计：创建人
    update_by      UUID,                                                -- 审计：更新人
    deleted_at     TIMESTAMPTZ,                                         -- 软删时间（NULL = 存活）

    CONSTRAINT fk_biz_cust_price_customer
        FOREIGN KEY (customer_id) REFERENCES biz_customer (id) ON DELETE RESTRICT,
    CONSTRAINT fk_biz_cust_price_material
        FOREIGN KEY (material_id) REFERENCES biz_material (id) ON DELETE RESTRICT,
    CONSTRAINT ck_biz_cust_price_price  CHECK (price > 0),                                  -- 卡面：价格必须 > 0
    CONSTRAINT ck_biz_cust_price_min_qty CHECK (min_qty >= 0),
    CONSTRAINT ck_biz_cust_price_status CHECK (status IN ('ACTIVE','INACTIVE')),
    CONSTRAINT ck_biz_cust_price_dates  CHECK (effective_to IS NULL OR effective_to >= effective_from)
);

COMMENT ON TABLE  biz_customer_price IS '客户协议价（ARCH-7 §15.3.2 ①；取价优先级最高）。一行 = 某客户对某物料、某起订量、某生效日起的含税协议单价';
COMMENT ON COLUMN biz_customer_price.customer_id    IS '客户（硬外键 biz_customer.id，ON DELETE RESTRICT：客户软删不影响本行，物理删会被本表挡住）';
COMMENT ON COLUMN biz_customer_price.material_id    IS '物料（硬外键 biz_material.id，ON DELETE RESTRICT）';
COMMENT ON COLUMN biz_customer_price.price          IS '含税协议单价 NUMERIC(18,4)，CHECK > 0；0 元不表示免费，赠品走单据 0 元行';
COMMENT ON COLUMN biz_customer_price.min_qty        IS '起订量（订购量 >= 本值才命中本行；0 = 不限制）。唯一键成员，NOT NULL 防 NULL 绕过唯一性';
COMMENT ON COLUMN biz_customer_price.effective_from IS '生效日（含）。唯一键成员，NOT NULL（默认今天）防 NULL 绕过唯一性';
COMMENT ON COLUMN biz_customer_price.effective_to   IS '失效日（含）；NULL = 长期有效。多条命中时按 min_qty 最大取最匹配的那条（ARCH-7 §15.3.3 ①）';
COMMENT ON COLUMN biz_customer_price.status         IS '状态域：ACTIVE 参与取价 / INACTIVE 停用（停用不删行，保留价格历史）';
COMMENT ON COLUMN biz_customer_price.deleted_at     IS '软删标记；软删后同一 (客户,物料,起订量,生效日) 可重新录入（唯一索引带 deleted_at IS NULL 谓词）';

-- 唯一索引：同一客户 + 物料 + 起订量 + 生效日 只能有一条**存活**价格
-- （软删后允许重录，与全仓 uk_biz_customer_code / uk_biz_sreturn_no 同口径）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cust_price
    ON biz_customer_price (customer_id, material_id, min_qty, effective_from)
    WHERE deleted_at IS NULL;
-- 「这个物料都有谁签了协议价」的物料维度回查（唯一索引前缀是 customer_id，覆盖不到）
CREATE INDEX IF NOT EXISTS idx_biz_cust_price_material
    ON biz_customer_price (material_id);
-- 客户价格台账页：按客户 + 状态筛
CREATE INDEX IF NOT EXISTS idx_biz_cust_price_customer_status
    ON biz_customer_price (customer_id, status) WHERE deleted_at IS NULL;


-- ---------- ② biz_price_tier（阶梯价 / 促销价：客户专属 + 通用） ----------
CREATE TABLE IF NOT EXISTS biz_price_tier (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),         -- 主键
    material_id    UUID          NOT NULL,                             -- 物料（硬外键 → biz_material.id）
    customer_id    UUID,                                                -- 客户（硬外键 → biz_customer.id；NULL = 适用于所有客户）
    tier_type      VARCHAR(16)   NOT NULL,                              -- 档位类型：TIER 阶梯 / PROMO 促销
    min_qty        NUMERIC(18,4) NOT NULL,                              -- 区间下界（含），唯一键成员
    max_qty        NUMERIC(18,4),                                       -- 区间上界（不含）；NULL = 无上限
    price          NUMERIC(18,4) NOT NULL,                              -- 含税单价（> 0）
    effective_from DATE          NOT NULL DEFAULT CURRENT_DATE,         -- 生效日（含），唯一键成员
    effective_to   DATE,                                                -- 失效日（含；NULL = 长期有效）
    status         VARCHAR(16)   NOT NULL DEFAULT 'ACTIVE',             -- 状态：ACTIVE 启用 / INACTIVE 停用
    remark         TEXT,                                                -- 备注
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,    -- 审计：创建时间
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,    -- 审计：更新时间（model 层维护）
    create_by      UUID,                                                -- 审计：创建人
    update_by      UUID,                                                -- 审计：更新人
    deleted_at     TIMESTAMPTZ,                                         -- 软删时间（NULL = 存活）

    CONSTRAINT fk_biz_price_tier_material
        FOREIGN KEY (material_id) REFERENCES biz_material (id) ON DELETE RESTRICT,
    CONSTRAINT fk_biz_price_tier_customer
        FOREIGN KEY (customer_id) REFERENCES biz_customer (id) ON DELETE RESTRICT,
    CONSTRAINT ck_biz_price_tier_type   CHECK (tier_type IN ('TIER','PROMO')),
    CONSTRAINT ck_biz_price_tier_price  CHECK (price > 0),                                      -- 卡面：价格必须 > 0
    CONSTRAINT ck_biz_price_tier_qty    CHECK (min_qty >= 0 AND (max_qty IS NULL OR max_qty > min_qty)),
    CONSTRAINT ck_biz_price_tier_status CHECK (status IN ('ACTIVE','INACTIVE')),
    CONSTRAINT ck_biz_price_tier_dates  CHECK (effective_to IS NULL OR effective_to >= effective_from)
);

COMMENT ON TABLE  biz_price_tier IS '阶梯价 / 促销价（ARCH-7 §15.3.2 ②③）。customer_id 非空 = 客户专属档位，NULL = 全客户通用档位';
COMMENT ON COLUMN biz_price_tier.material_id    IS '物料（硬外键 biz_material.id，ON DELETE RESTRICT）';
COMMENT ON COLUMN biz_price_tier.customer_id    IS '客户（硬外键 biz_customer.id，ON DELETE RESTRICT）；NULL = 适用于所有客户（唯一索引用 NULLS NOT DISTINCT 管重复）';
COMMENT ON COLUMN biz_price_tier.tier_type      IS '档位类型：TIER=阶梯量价 / PROMO=促销价（同一物料可同时存在两类，取价按 ARCH-7 §15.3.3 分优先级）';
COMMENT ON COLUMN biz_price_tier.min_qty        IS '区间下界（含）：订购量 >= 本值才命中';
COMMENT ON COLUMN biz_price_tier.max_qty        IS '区间上界（不含）：订购量 < 本值才命中；NULL = 无上限。CHECK 保证严格大于 min_qty（相等即空区间）';
COMMENT ON COLUMN biz_price_tier.price          IS '含税单价 NUMERIC(18,4)，CHECK > 0';
COMMENT ON COLUMN biz_price_tier.status         IS '状态域：ACTIVE 参与取价 / INACTIVE 停用';

-- 唯一索引：同物料 + 同客户（NULL 也参与判定）+ 同档位类型 + 同起订量 + 同生效日 只能一条存活
-- NULLS NOT DISTINCT：通用档位（customer_id IS NULL）也受唯一性保护，避免「并列档位两条价」
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_price_tier
    ON biz_price_tier (material_id, customer_id, tier_type, min_qty, effective_from)
    NULLS NOT DISTINCT
    WHERE deleted_at IS NULL;
-- 取价扫描路径：WHERE material_id = ? AND tier_type = ? AND status = 'ACTIVE' AND min_qty <= ?
CREATE INDEX IF NOT EXISTS idx_biz_price_tier_lookup
    ON biz_price_tier (material_id, tier_type, status, min_qty) WHERE deleted_at IS NULL;
-- 客户专属档位回查（客户价格台账页）；通用档位（NULL）不在本索引里
CREATE INDEX IF NOT EXISTS idx_biz_price_tier_customer
    ON biz_price_tier (customer_id, material_id) WHERE customer_id IS NOT NULL AND deleted_at IS NULL;


-- ---------- ③ 物料标准售价（ARCH-7 §15.3.4 表改动） ----------
ALTER TABLE biz_material
    ADD COLUMN IF NOT EXISTS standard_price NUMERIC(18,4);

COMMENT ON COLUMN biz_material.standard_price IS
    '物料标准售价（含税，ARCH-7 §15.3.4）。NULL = 未维护（取价链走到这里仍空则前端手填）；'
    '非空必须 > 0。与 costing_method=STANDARD 的标准**成本**无关，后者在 biz_inventory.avg_cost / 成本核算里';

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_material_standard_price') THEN
        ALTER TABLE biz_material
            ADD CONSTRAINT ck_biz_material_standard_price
            CHECK (standard_price IS NULL OR standard_price > 0);
    END IF;
END $$;


-- ======================================================
-- 自检（末尾 DO 块）：结构 + 语义 + 行为探针，任一不符 → 整个迁移失败（事务回滚）
-- ======================================================
DO $$
DECLARE
    n            bigint;
    d            text;
    probe_cus    uuid := 'f0270000-0000-4000-8000-000000000001'::uuid;
    probe_mat    uuid := 'f0270000-0000-4000-8000-000000000002'::uuid;
    probe_price  uuid := 'f0270000-0000-4000-8000-000000000003'::uuid;
    probe_tier   uuid := 'f0270000-0000-4000-8000-000000000004'::uuid;
    blocked      boolean;
    i            int;
BEGIN
    -- 4.1 三张对象在位
    IF to_regclass('public.biz_customer_price') IS NULL OR to_regclass('public.biz_price_tier') IS NULL THEN
        RAISE EXCEPTION 'V0270 自检失败：biz_customer_price / biz_price_tier 未建全';
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_material' AND column_name = 'standard_price';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0270 自检失败：biz_material.standard_price 列缺失';
    END IF;

    -- 4.2 列数逐表比对（多一列少一列都算漂移）
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_customer_price';
    IF n <> 14 THEN
        RAISE EXCEPTION 'V0270 自检失败：biz_customer_price 列数 = %（期望 14）', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_price_tier';
    IF n <> 16 THEN
        RAISE EXCEPTION 'V0270 自检失败：biz_price_tier 列数 = %（期望 16）', n;
    END IF;

    -- 4.3 卡面点名的列必须逐字在位
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_customer_price'
       AND column_name IN ('id','customer_id','material_id','price','min_qty','effective_from',
                           'effective_to','status','remark','created_at','updated_at',
                           'create_by','update_by','deleted_at');
    IF n <> 14 THEN
        RAISE EXCEPTION 'V0270 自检失败：biz_customer_price 卡面列只齐了 %/14', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_price_tier'
       AND column_name IN ('id','material_id','customer_id','tier_type','min_qty','max_qty',
                           'price','effective_from','effective_to','status','remark',
                           'created_at','updated_at','create_by','update_by','deleted_at');
    IF n <> 16 THEN
        RAISE EXCEPTION 'V0270 自检失败：biz_price_tier 卡面列只齐了 %/16', n;
    END IF;

    -- 4.4 金额列必须是 NUMERIC(18,4)（设计稿口径，别照单据行改成 18,6）
    SELECT string_agg(table_name || '.' || column_name || '=' ||
                      data_type || '(' || numeric_precision || ',' || numeric_scale || ')', '; ')
      INTO d
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND ((table_name = 'biz_customer_price' AND column_name IN ('price','min_qty'))
         OR (table_name = 'biz_price_tier'     AND column_name IN ('price','min_qty','max_qty'))
         OR (table_name = 'biz_material'       AND column_name = 'standard_price'))
       AND NOT (data_type = 'numeric' AND numeric_precision = 18 AND numeric_scale = 4);
    IF d IS NOT NULL THEN
        RAISE EXCEPTION 'V0270 自检失败：价格列不是 NUMERIC(18,4)（%）', d;
    END IF;

    -- 4.5 唯一索引语义：uk_biz_cust_price 四列齐全 + 存活行唯一
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_customer_price' AND indexname = 'uk_biz_cust_price';
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0270 自检失败：唯一索引 uk_biz_cust_price 缺失（是否与既有索引撞名被静默跳过？）';
    END IF;
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%customer_id%' OR d NOT LIKE '%material_id%'
       OR d NOT LIKE '%min_qty%' OR d NOT LIKE '%effective_from%' OR d NOT LIKE '%deleted_at IS NULL%' THEN
        RAISE EXCEPTION 'V0270 自检失败：uk_biz_cust_price 口径不对（def=%）', d;
    END IF;

    -- 4.6 唯一索引语义：uk_biz_price_tier 必须 NULLS NOT DISTINCT（否则通用档位可重复）
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_price_tier' AND indexname = 'uk_biz_price_tier';
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0270 自检失败：唯一索引 uk_biz_price_tier 缺失';
    END IF;
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%NULLS NOT DISTINCT%' OR d NOT LIKE '%deleted_at IS NULL%' THEN
        RAISE EXCEPTION 'V0270 自检失败：uk_biz_price_tier 不是 NULLS NOT DISTINCT 的存活行唯一索引（def=%）', d;
    END IF;

    -- 4.7 四 + 四 + 一条 CHECK 在位
    FOREACH d IN ARRAY ARRAY['ck_biz_cust_price_price','ck_biz_cust_price_min_qty',
                              'ck_biz_cust_price_status','ck_biz_cust_price_dates',
                              'ck_biz_price_tier_type','ck_biz_price_tier_price',
                              'ck_biz_price_tier_qty','ck_biz_price_tier_status',
                              'ck_biz_price_tier_dates','ck_biz_material_standard_price']
    LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = d) THEN
            RAISE EXCEPTION 'V0270 自检失败：约束 % 缺失', d;
        END IF;
    END LOOP;
    FOREACH d IN ARRAY ARRAY['TIER','PROMO']
    LOOP
        IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_price_tier_type')
           NOT LIKE '%' || d || '%' THEN
            RAISE EXCEPTION 'V0270 自检失败：ck_biz_price_tier_type 缺域值 %', d;
        END IF;
    END LOOP;
    FOREACH d IN ARRAY ARRAY['ACTIVE','INACTIVE']
    LOOP
        IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_cust_price_status')
           NOT LIKE '%' || d || '%' THEN
            RAISE EXCEPTION 'V0270 自检失败：ck_biz_cust_price_status 缺域值 %', d;
        END IF;
    END LOOP;

    -- 4.8 外键口径：两表各 2 条，全部 RESTRICT（物理删主数据必须被挡住，不能吞价格）
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_customer_price'::regclass AND contype = 'f';
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0270 自检失败：biz_customer_price 外键条数 = %（期望 2：customer + material）', n;
    END IF;
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_price_tier'::regclass AND contype = 'f';
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0270 自检失败：biz_price_tier 外键条数 = %（期望 2：material + customer）', n;
    END IF;
    SELECT string_agg(conname || '→' || pg_get_constraintdef(oid), '; ') INTO d
      FROM pg_constraint
     WHERE conrelid IN ('public.biz_customer_price'::regclass, 'public.biz_price_tier'::regclass)
       AND contype = 'f'
       AND (pg_get_constraintdef(oid) NOT LIKE '%ON DELETE RESTRICT%');
    IF d IS NOT NULL THEN
        RAISE EXCEPTION 'V0270 自检失败：存在非 RESTRICT 外键（%）', d;
    END IF;

    -- 4.9 默认值：status='ACTIVE'、min_qty=0、effective_from 有默认
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public'
       AND ((table_name = 'biz_customer_price' AND column_name IN ('status','min_qty','effective_from'))
         OR (table_name = 'biz_price_tier'     AND column_name IN ('status','effective_from')))
       AND column_default IS NULL;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0270 自检失败：有 % 个应带默认值的列没有默认值', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public'
       AND ((table_name = 'biz_customer_price' AND column_name = 'status')
         OR (table_name = 'biz_price_tier'     AND column_name = 'status'))
       AND column_default LIKE '''ACTIVE''%';
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0270 自检失败：status 默认值不是 ACTIVE（齐了 %/2）', n;
    END IF;

    -- 4.10 行为探针准备：探针客户 + 探针物料（外键要求真实父行）
    INSERT INTO biz_customer (id, code, name) VALUES (probe_cus, '__V0270_PROBE_CUS__', 'V0270 自检探针客户');
    INSERT INTO biz_material (id, material_code, material_name, unit_id)
    VALUES (probe_mat, '__V0270_PROBE_MAT__', 'V0270 自检探针物料', gen_random_uuid());

    -- 4.10a 价格 = 0 → 必须被拒（卡面：价格必须 > 0）
    BEGIN
        INSERT INTO biz_customer_price (customer_id, material_id, price)
        VALUES (probe_cus, probe_mat, 0);
        RAISE EXCEPTION 'V0270 自检失败：price = 0 未被 ck_biz_cust_price_price 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.10b 价格 < 0 → 必须被拒
    BEGIN
        INSERT INTO biz_customer_price (customer_id, material_id, price)
        VALUES (probe_cus, probe_mat, -1);
        RAISE EXCEPTION 'V0270 自检失败：price = -1 未被 ck_biz_cust_price_price 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.10c 起订量 < 0 → 必须被拒
    BEGIN
        INSERT INTO biz_customer_price (customer_id, material_id, price, min_qty)
        VALUES (probe_cus, probe_mat, 10, -1);
        RAISE EXCEPTION 'V0270 自检失败：min_qty = -1 未被 ck_biz_cust_price_min_qty 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.10d 状态越界 → 必须被拒
    BEGIN
        INSERT INTO biz_customer_price (customer_id, material_id, price, status)
        VALUES (probe_cus, probe_mat, 10, 'EXPIRED');
        RAISE EXCEPTION 'V0270 自检失败：status = EXPIRED 未被 ck_biz_cust_price_status 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.10e 失效日早于生效日 → 必须被拒
    BEGIN
        INSERT INTO biz_customer_price (customer_id, material_id, price, effective_from, effective_to)
        VALUES (probe_cus, probe_mat, 10, DATE '2026-06-01', DATE '2026-05-31');
        RAISE EXCEPTION 'V0270 自检失败：effective_to < effective_from 未被 ck_biz_cust_price_dates 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.10f 物料不存在 → 必须被外键拦住
    BEGIN
        INSERT INTO biz_customer_price (customer_id, material_id, price)
        VALUES (probe_cus, '00000000-0000-4000-8000-0000ffff0270'::uuid, 10);
        RAISE EXCEPTION 'V0270 自检失败：material_id 指向不存在的物料却插入成功';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.10g 正常行 + 默认值探针（不传 min_qty / effective_from / status）
    INSERT INTO biz_customer_price (id, customer_id, material_id, price)
    VALUES (probe_price, probe_cus, probe_mat, 123.4567);
    SELECT min_qty::text || '|' || status || '|' || (effective_from = CURRENT_DATE)::text
      INTO d FROM biz_customer_price WHERE id = probe_price;
    IF d <> '0.0000|ACTIVE|true' THEN
        RAISE EXCEPTION 'V0270 自检失败：默认值落库 = %（期望 0.0000|ACTIVE|true）', d;
    END IF;

    -- 4.10h 唯一索引：同客户/物料/起订量/生效日 再插一条 → 必须被拒
    BEGIN
        INSERT INTO biz_customer_price (customer_id, material_id, price)
        VALUES (probe_cus, probe_mat, 99);
        RAISE EXCEPTION 'V0270 自检失败：uk_biz_cust_price 未拦住同键重复行';
    EXCEPTION WHEN unique_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.10i 软删后同键可重录（部分唯一索引语义）
    UPDATE biz_customer_price SET deleted_at = now() WHERE id = probe_price;
    INSERT INTO biz_customer_price (customer_id, material_id, price)
    VALUES (probe_cus, probe_mat, 100);
    SELECT count(*) INTO n FROM biz_customer_price
     WHERE customer_id = probe_cus AND material_id = probe_mat AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0270 自检失败：软删后同键重录的存活行 = %（期望 1）', n;
    END IF;

    -- 4.10j 档位表：max_qty <= min_qty → 必须被拒
    BEGIN
        INSERT INTO biz_price_tier (material_id, tier_type, min_qty, max_qty, price)
        VALUES (probe_mat, 'TIER', 10, 10, 8);
        RAISE EXCEPTION 'V0270 自检失败：max_qty = min_qty 未被 ck_biz_price_tier_qty 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.10k 档位表：价格 0 → 必须被拒
    BEGIN
        INSERT INTO biz_price_tier (material_id, tier_type, min_qty, price)
        VALUES (probe_mat, 'PROMO', 0, 0);
        RAISE EXCEPTION 'V0270 自检失败：档位 price = 0 未被 ck_biz_price_tier_price 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.10l 档位类型越界 → 必须被拒
    BEGIN
        INSERT INTO biz_price_tier (material_id, tier_type, min_qty, price)
        VALUES (probe_mat, 'RETAIL', 0, 8);
        RAISE EXCEPTION 'V0270 自检失败：tier_type = RETAIL 未被 ck_biz_price_tier_type 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.10m 通用档位（customer_id IS NULL）重复 → 必须被 NULLS NOT DISTINCT 唯一索引拦住
    INSERT INTO biz_price_tier (id, material_id, customer_id, tier_type, min_qty, max_qty, price)
    VALUES (probe_tier, probe_mat, NULL, 'TIER', 0, 100, 10);
    BEGIN
        INSERT INTO biz_price_tier (material_id, customer_id, tier_type, min_qty, max_qty, price)
        VALUES (probe_mat, NULL, 'TIER', 0, 100, 9);
        RAISE EXCEPTION 'V0270 自检失败：通用档位（customer_id IS NULL）可重复插入（NULLS NOT DISTINCT 未生效）';
    EXCEPTION WHEN unique_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.10n 客户专属档位与通用档位互不冲突（同一物料同档位可以同时存在两条）
    INSERT INTO biz_price_tier (material_id, customer_id, tier_type, min_qty, max_qty, price)
    VALUES (probe_mat, probe_cus, 'TIER', 0, 100, 7);
    SELECT count(*) INTO n FROM biz_price_tier
     WHERE material_id = probe_mat AND tier_type = 'TIER' AND deleted_at IS NULL;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0270 自检失败：通用档位 + 客户专属档位应有 2 行，实际 %', n;
    END IF;

    -- 4.10o 物料标准售价：NULL 允许、> 0 允许、0 / 负数被拒
    UPDATE biz_material SET standard_price = NULL WHERE id = probe_mat;
    UPDATE biz_material SET standard_price = 12345.6789 WHERE id = probe_mat;
    SELECT standard_price::text INTO d FROM biz_material WHERE id = probe_mat;
    IF d <> '12345.6789' THEN
        RAISE EXCEPTION 'V0270 自检失败：standard_price 读回 = %（期望 12345.6789）', d;
    END IF;
    FOR i IN 0..1 LOOP
        BEGIN
            UPDATE biz_material
               SET standard_price = CASE WHEN i = 0 THEN 0 ELSE -0.0001 END
             WHERE id = probe_mat;
            RAISE EXCEPTION 'V0270 自检失败：standard_price <= 0 未被 ck_biz_material_standard_price 拒绝（case=%）', i;
        EXCEPTION WHEN check_violation THEN
            NULL;   -- 期望路径
        END;
    END LOOP;
    UPDATE biz_material SET standard_price = NULL WHERE id = probe_mat;   -- 复原

    -- 4.10p 外键 RESTRICT：还挂着价格行的物料/客户不能被物理删
    --        注意 SQLSTATE：`ON DELETE RESTRICT` 抛的是 **23001 restrict_violation**，
    --        不是 23503 foreign_key_violation（NO ACTION 才是 23503）。两个都接住，
    --        否则自检块会被 RESTRICT 顶穿成一条「迁移失败」的假故障。
    blocked := FALSE;
    BEGIN
        DELETE FROM biz_material WHERE id = probe_mat;
    EXCEPTION WHEN foreign_key_violation OR restrict_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0270 自检失败：物料仍挂着价格行却能被物理删除（应为 ON DELETE RESTRICT）';
    END IF;
    blocked := FALSE;
    BEGIN
        DELETE FROM biz_customer WHERE id = probe_cus;
    EXCEPTION WHEN foreign_key_violation OR restrict_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0270 自检失败：客户仍挂着协议价却能被物理删除（应为 ON DELETE RESTRICT）';
    END IF;

    -- 4.11 清场：探针行全部消失
    DELETE FROM biz_customer_price WHERE customer_id = probe_cus;
    DELETE FROM biz_price_tier     WHERE material_id = probe_mat;
    DELETE FROM biz_material       WHERE id = probe_mat;
    DELETE FROM biz_customer       WHERE id = probe_cus;
    SELECT (SELECT count(*) FROM biz_customer_price WHERE customer_id = probe_cus OR material_id = probe_mat)
         + (SELECT count(*) FROM biz_price_tier     WHERE material_id = probe_mat)
         + (SELECT count(*) FROM biz_material       WHERE id = probe_mat)
         + (SELECT count(*) FROM biz_customer       WHERE id = probe_cus)
      INTO n;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0270 自检失败：探针残留 % 行', n;
    END IF;
    IF EXISTS (SELECT 1 FROM biz_material WHERE material_code = '__V0270_PROBE_MAT__' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0270 自检失败：探针物料残留';
    END IF;

    RAISE NOTICE 'V0270 自检通过：biz_customer_price(14 列) / biz_price_tier(16 列) / biz_material.standard_price 就位，唯一索引与 RESTRICT 外键行为已验证';
END $$;
