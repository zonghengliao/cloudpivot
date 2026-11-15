-- ======================================================
-- V0272：多币种主数据 —— 币种表 + 汇率表（SO-39 · ARCH-7 §15.6.3）
--
--   ① biz_currency       币种主数据（CNY 本位 / USD / EUR / HKD 种子）
--   ② biz_exchange_rate  汇率表（SPOT 即时 / MONTH_END 月末），按月维护
--
-- 本卡只落**存储层**（表 / 约束 / 索引 / 种子）。双币字段（sales_order / receivable /
-- receipt / invoice 各加 currency_code + exchange_rate + 三个 base_* 列）与汇率锁定属
-- 下一张卡 SO-40；因此这里**不**新增 Rust 代码、不注册 DocKind / LEGAL_EDGES / 菜单权限码
-- —— 币种与汇率是主数据，没有状态机迁移边（status 只是 ACTIVE/INACTIVE 开关）。
--
-- ── 号段（开工实测复核）──────────────────────────────────
-- 卡面点名 `V0272__sales_currency`。开工实测：backend/migrations/business/ 最大号 = V0313
-- （V0313__customer_credit_used，SO-38 提交 08cc778），**V0272 未被任何迁移占用**
-- （V0271–V0279 是销售链的一整段空洞；V0270 被 SO-32 价格两表占用），故按卡面逐字取 V0272，
-- 不走「最大号 +1」。乱序补迁在本仓是**允许**的：alun-db 的 `apply()` 对「版本号低于已应用
-- 最大值」的 versioned 迁移只打一条 `WARN …（乱序补迁）`，不拒绝、不阻断（与 sqlx 同行为，
-- 先例见 V0270 头注释与 docs/evidence/20261107-so32-price-migration/）。
-- 本文件只**新建两张主数据表**、不引用任何更高号才出现的对象，空库重放时天然排在
-- V0271 之后、V0280 之前，次序上不需要谁先跑。
-- 提交前再 `ls backend/migrations/business/*.up.sql | sort | tail -3` 复核一次，防并行卡抢号。
--
-- ── 币种表口径（ARCH-7 §15.6.3 ①，逐字对齐）──────────────
--  1) `code` 直接做 PK（varchar(8)），CHECK `^[A-Z]{3}$`：ISO 4217 三位大写。
--     与既有 V0137 的 `ck_biz_contract_currency` 同一正则口径 —— 币种码是**跨模块的对外标识**，
--     大小写/长度不一致会让 `contract.currency_code = currency.code` 这类 join 静默不命中。
--  2) `decimal_places` CHECK `BETWEEN 0 AND 4`：本仓所有金额列统一 NUMERIC(18,4)
--     （biz_sales_order.total_with_tax / biz_voucher_entry.debit_amount …），币种小数位
--     超过 4 位时四舍五入会在「原币 × 汇率 = 本位币」的链路上静默丢分。
--     0 合法（JPY/KRW 无小数），3 合法（KWD/BHD），4 是本仓上限。
--  3) `is_base` 的部分唯一索引 `uk_biz_currency_single_base`：
--     `CREATE UNIQUE INDEX … ON biz_currency (is_base) WHERE is_base` ——
--     索引只收 `is_base = true` 的行，而这些行在该列上全部相等 ⇒ 第二行必然撞唯一性。
--     ARCH §15.6.4 ③「本位币只能有一行」由库层硬保证，不靠应用层自觉。
--  4) 不做 `deleted_at` 软删：币种被汇率表 / 单据历史以外键与快照引用，
--     物理删会毁掉历史可读性；「停用」用 `status = 'INACTIVE'` 表达（与 biz_currency 的
--     定位一致：它是 4~10 行的极小主数据，不是业务实体）。
--     审计字段保留 created_at / updated_at / create_by / update_by。
--
-- ── 汇率表口径（ARCH-7 §15.6.3 ②）──────────────────────
--  1) `rate NUMERIC(18,8)` CHECK `> 0`：**卡面硬要求**。0 与负数一律拒（0 会让整单折成
--     0 元、负数会让应收变成反向应收，两者都比「没录汇率」危险得多）。
--     精度 8 位：JPY→CNY 这类 0.04x 的量级要留够有效位，18,8 也是 ARCH 稿面逐字给的口径。
--  2) 方向：**1 单位 from_currency = rate 单位 to_currency**（`from → to`）。
--     本表只存「非本位币 → 本位币」方向的业务汇率；反向（本位币 → 外币）需要时由
--     1 / rate 推导，**不落重复行**（两行互为倒数必然因为四舍五入而对不上，是假账源头）。
--     因此 CHECK `from_currency <> to_currency`：自反汇率恒为 1，落库只会让
--     「取 <= 某日的最新一条」多一条噪声行。
--  3) `rate_type` NOT NULL DEFAULT 'SPOT'：唯一索引成员，**必须 NOT NULL** ——
--     PG 里 NULL 彼此不相等，可空列会让「同币对同类型同生效日」插进第二条且不报错，
--     汇率从此两个真相。DEFAULT 只是录入便利，不是可空。
--  4) `effective_date` NOT NULL **且不给 DEFAULT**：汇率生效日必须显式给。
--     给 `DEFAULT CURRENT_DATE` 会让「补录上月月末汇率」静默按今天生效，
--     月末重估取数直接错月，而界面上看不出任何异常。
--     语义：生效日**含当日**；取汇率 = `effective_date <= 目标日 ORDER BY effective_date DESC LIMIT 1`
--     （SO-40 的汇率锁定按单据日期取，**锁死**进单据，后续汇率变动不影响已建单据）。
--  5) 唯一索引 `uk_biz_exchange_rate (from_currency, to_currency, rate_type, effective_date)`
--     —— 名字与列序逐字照卡面（下游 upsert 会写
--     `ON CONFLICT (from_currency, to_currency, rate_type, effective_date) DO UPDATE`）。
--     四列全部 NOT NULL，唯一性才是真的。该索引同时充当查汇率的覆盖索引
--     （最左前缀 from+to+type，再按 effective_date 倒序扫），不另建查询索引。
--  6) 外键：`from_currency` 与 `to_currency` **都**建硬外键 → biz_currency(code) ON DELETE RESTRICT。
--     ARCH 稿面只给 from_currency 标了 `FK →`（to_currency 注释成「本位币」），本卡多补
--     to_currency 的 FK：它是同一个币种主数据的引用，不建外键就能录出「兑 XXX 不存在币种」
--     的孤儿汇率，而月末重估会静默跳过它 —— 少一行外键换来的风险不对称。
--     主数据走 RESTRICT（币种表本身不删，见上），实际几乎不触发；真触发说明有人在物理删
--     被引用的币种，**应当**被挡住。这是主数据配置表而非单据（ADR-06「单据链零外键」不管这里，
--     先例 V0138 / V0260 / V0270）。
--  7) 不写触发器：本仓迁移零 TRIGGER，跨表校验（例如「只允许非本位币 → 本位币」）留给
--     写入口（SO-40 的汇率维护接口）在事务里做；库层只保证单表可判定的硬约束。
--
-- ── 种子（卡面：CNY(base) / USD / EUR / HKD）──────────────
--   ON CONFLICT (code) DO NOTHING：重复执行 no-op，已存在（可能被人改过 name/symbol）的行不动。
--   种子行的 `is_base` 是**现算**的（`v.is_base AND NOT EXISTS(已有本位币)`），不是写死 true：
--   写死 true 在「库中已另立本位币（USD）且 CNY 行被删」时会撞部分唯一索引、迁移直接失败。
--   本位币兜底：若库里已无本位币（例如有人手工把 is_base 全关了），把 CNY 补成本位币；
--   若库里已另立本位币（USD base），**不覆盖** —— 本位币唯一是硬约束，重放本迁移
--   不该抢别人的账套口径（这种情况下 seed 只补缺失币种）。
--
-- ── 幂等 ──────────────────────────────────────────────
--   CREATE TABLE / INDEX IF NOT EXISTS；种子 ON CONFLICT DO NOTHING；
--   末尾 DO 块自检可反复执行（探针自清场，残留必须为 0）。
-- ======================================================

-- ---------- ① 币种主数据 ----------
CREATE TABLE IF NOT EXISTS biz_currency (
    code           VARCHAR(8)  PRIMARY KEY,                        -- 币种码（ISO 4217 三位大写，如 CNY / USD）
    name           VARCHAR(64) NOT NULL,                           -- 中文名称（人民币 / 美元 …）
    symbol         VARCHAR(8)  NOT NULL,                           -- 符号（¥ / $ / € / HK$）
    decimal_places INTEGER     NOT NULL DEFAULT 2,                 -- 小数位（0..4；0=JPY，3=KWD）
    is_base        BOOLEAN     NOT NULL DEFAULT false,             -- 本位币（全表最多一行为 true，部分唯一索引保证）
    status         VARCHAR(16) NOT NULL DEFAULT 'ACTIVE',          -- 状态：ACTIVE 可用 / INACTIVE 停用（新单据不可选）
    remark         TEXT,                                           -- 备注
    created_at     TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：创建时间
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：更新时间（维护方负责回写）
    create_by      UUID,                                           -- 审计：创建人
    update_by      UUID,                                           -- 审计：更新人

    CONSTRAINT ck_biz_currency_code CHECK (code ~ '^[A-Z]{3}$'),
    CONSTRAINT ck_biz_currency_decimal_places CHECK (decimal_places BETWEEN 0 AND 4),
    CONSTRAINT ck_biz_currency_status CHECK (status IN ('ACTIVE','INACTIVE'))
);

COMMENT ON TABLE  biz_currency IS '币种主数据（ARCH-7 §15.6.3 ①）。极小主数据（本仓 4 行），无软删：停用走 status=INACTIVE';
COMMENT ON COLUMN biz_currency.code           IS '币种码，PK。ISO 4217 三位大写（CHECK ^[A-Z]{3}$），与 biz_contract.currency_code 同口径';
COMMENT ON COLUMN biz_currency.decimal_places IS '小数位 0..4。本仓金额列统一 NUMERIC(18,4)，>4 会在「原币 × 汇率 = 本位币」链路上静默丢分';
COMMENT ON COLUMN biz_currency.is_base        IS '本位币标记：全表最多一行为 true（部分唯一索引 uk_biz_currency_single_base）；ARCH §15.6.4 ③';
COMMENT ON COLUMN biz_currency.status         IS '状态域：ACTIVE 新单据可选 / INACTIVE 停用（历史单据与汇率行保留，仍可查询与对账）';

-- 本位币唯一：ARCH §15.6.4 ③「biz_currency 中 is_base=true 只能有一行」。
-- 部分唯一索引只索引 is_base=true 的行，这些行在该列上全部相等 ⇒ 第二条必然 unique_violation。
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_currency_single_base
    ON biz_currency (is_base)
    WHERE is_base;

-- ---------- ② 汇率表（按月维护，月末汇率结账用） ----------
CREATE TABLE IF NOT EXISTS biz_exchange_rate (
    id             UUID          PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    from_currency  VARCHAR(8)    NOT NULL,                             -- 原币（非本位币；硬外键 → biz_currency.code）
    to_currency    VARCHAR(8)    NOT NULL,                             -- 目标币种（业务上 = 本位币；硬外键 → biz_currency.code）
    rate           NUMERIC(18,8) NOT NULL,                             -- 1 单位 from = rate 单位 to；CHECK > 0
    rate_type      VARCHAR(16)   NOT NULL DEFAULT 'SPOT',              -- SPOT 即时 / MONTH_END 月末（唯一索引成员，NOT NULL）
    effective_date DATE          NOT NULL,                             -- 生效日（含）。唯一索引成员，NOT NULL 且**无默认**（见头注释 ②.4）
    remark         TEXT,                                               -- 备注（来源：中国银行 / 央行中间价 …）
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,   -- 审计：创建时间
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,   -- 审计：更新时间
    create_by      UUID,                                               -- 审计：创建人
    update_by      UUID,                                               -- 审计：更新人

    CONSTRAINT fk_biz_exchange_rate_from
        FOREIGN KEY (from_currency) REFERENCES biz_currency (code) ON DELETE RESTRICT,
    CONSTRAINT fk_biz_exchange_rate_to
        FOREIGN KEY (to_currency) REFERENCES biz_currency (code) ON DELETE RESTRICT,
    CONSTRAINT ck_biz_exchange_rate_rate CHECK (rate > 0),                      -- 卡面硬要求：汇率必须 > 0
    CONSTRAINT ck_biz_exchange_rate_type CHECK (rate_type IN ('SPOT','MONTH_END')),
    CONSTRAINT ck_biz_exchange_rate_pair CHECK (from_currency <> to_currency)   -- 自反汇率恒 1，落库只会污染取数
);

COMMENT ON TABLE  biz_exchange_rate IS '汇率表（ARCH-7 §15.6.3 ②）。方向：1 单位 from_currency = rate 单位 to_currency；只存「外币 → 本位币」，反向取倒数不落重复行';
COMMENT ON COLUMN biz_exchange_rate.from_currency  IS '原币（硬外键 biz_currency.code，ON DELETE RESTRICT）';
COMMENT ON COLUMN biz_exchange_rate.to_currency    IS '目标币种，业务上即本位币（硬外键 biz_currency.code，ON DELETE RESTRICT）';
COMMENT ON COLUMN biz_exchange_rate.rate           IS '汇率 NUMERIC(18,8)，CHECK > 0；0 会把整单折成 0 元、负数会造出反向应收，两者比「没录汇率」更危险';
COMMENT ON COLUMN biz_exchange_rate.rate_type      IS '汇率类型：SPOT 即时 / MONTH_END 月末（ARCH §15.6.4 ② 月末重估用 MONTH_END）。唯一索引成员，NOT NULL';
COMMENT ON COLUMN biz_exchange_rate.effective_date IS '生效日（含）。取汇率 = effective_date <= 目标日 ORDER BY effective_date DESC LIMIT 1；不带 DEFAULT，避免补录历史汇率静默按今天生效';

-- 卡面唯一索引（名字与列序逐字对齐 upsert 写法）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_exchange_rate
    ON biz_exchange_rate (from_currency, to_currency, rate_type, effective_date);

-- ---------- ③ 种子：4 个币种（CNY 为本位币） ----------
-- `is_base` 用 `NOT EXISTS(已有本位币)` 现算，而不是写死 true：
--   若库里已另立本位币（例如有人把 USD 设成本位币、同时删掉了 CNY 行），写死 true 会让
--   本迁移撞 `uk_biz_currency_single_base` 直接失败。现算之后：有本位币 ⇒ CNY 以非本位币种补录；
--   没有本位币 ⇒ CNY 立为本位币。两条路径都不会抛错，也不会抢别人的账套口径。
INSERT INTO biz_currency (code, name, symbol, decimal_places, is_base, status, remark)
SELECT v.code, v.name, v.symbol, v.decimal_places,
       v.is_base AND NOT EXISTS (SELECT 1 FROM biz_currency WHERE is_base),
       'ACTIVE', v.remark
  FROM (VALUES
        ('CNY', '人民币', '¥',  2, true,  '本位币（ARCH-7 §15.6.3 种子）'),
        ('USD', '美元',   '$',  2, false, 'ARCH-7 §15.6.3 种子'),
        ('EUR', '欧元',   '€',  2, false, 'ARCH-7 §15.6.3 种子'),
        ('HKD', '港币',   'HK$', 2, false, 'ARCH-7 §15.6.3 种子')
       ) AS v(code, name, symbol, decimal_places, is_base, remark)
ON CONFLICT (code) DO NOTHING;

-- 本位币兜底：库里一行本位币都没有时，把 CNY 立为本位币（重放 / 人工误清都能自愈）。
-- 已有本位币（哪怕不是 CNY）则不动 —— 本位币唯一是硬约束，迁移不抢账套口径。
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM biz_currency WHERE is_base) THEN
        UPDATE biz_currency SET is_base = true, updated_at = now() WHERE code = 'CNY';
        RAISE NOTICE 'SO-39: 库中无本位币，已把 CNY 立为本位币';
    END IF;
END $$;

-- ---------- ④ 迁移自检（结构 + 种子 + 行为探针，可反复执行） ----------
DO $$
DECLARE
    n   integer;
    txt text;
BEGIN
    -- ===== 结构：列数 =====
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_currency';
    IF n <> 11 THEN
        RAISE EXCEPTION 'SO-39: biz_currency 列数 = %（期望 11：code/name/symbol/decimal_places/is_base/status/remark + 4 审计列）', n;
    END IF;

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_exchange_rate';
    IF n <> 11 THEN
        RAISE EXCEPTION 'SO-39: biz_exchange_rate 列数 = %（期望 11：id/from/to/rate/rate_type/effective_date/remark + 4 审计列）', n;
    END IF;

    -- ===== 结构：关键列型（精度必须与 ARCH 稿面逐字一致）=====
    SELECT data_type INTO txt FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_currency' AND column_name = 'decimal_places';
    IF txt <> 'integer' THEN
        RAISE EXCEPTION 'SO-39: decimal_places 类型 = %（期望 integer）', txt;
    END IF;

    SELECT data_type INTO txt FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_currency' AND column_name = 'is_base';
    IF txt <> 'boolean' THEN
        RAISE EXCEPTION 'SO-39: is_base 类型 = %（期望 boolean）', txt;
    END IF;

    SELECT format('%s,%s', numeric_precision, numeric_scale) INTO txt
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_exchange_rate' AND column_name = 'rate';
    IF txt <> '18,8' THEN
        RAISE EXCEPTION 'SO-39: rate 精度 = %（期望 18,8；写宽了会静默丢有效位，写窄了会四舍五入）', txt;
    END IF;

    -- ===== 结构：本位币部分唯一索引必须在位且谓词正确 =====
    IF to_regclass('public.uk_biz_currency_single_base') IS NULL THEN
        RAISE EXCEPTION 'SO-39: 缺本位币唯一索引 uk_biz_currency_single_base（ARCH §15.6.4 ③）';
    END IF;
    SELECT indexdef INTO txt FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_currency_single_base';
    IF txt NOT LIKE 'CREATE UNIQUE INDEX%' OR txt NOT LIKE '%(is_base)%' OR txt NOT LIKE '%WHERE is_base%' THEN
        RAISE EXCEPTION 'SO-39: uk_biz_currency_single_base 定义不对（必须是 UNIQUE (is_base) WHERE is_base）：%', txt;
    END IF;

    -- ===== 结构：汇率唯一索引四列齐 =====
    IF to_regclass('public.uk_biz_exchange_rate') IS NULL THEN
        RAISE EXCEPTION 'SO-39: 缺汇率唯一索引 uk_biz_exchange_rate（卡面点名）';
    END IF;
    SELECT indexdef INTO txt FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_exchange_rate';
    IF txt NOT LIKE 'CREATE UNIQUE INDEX%'
       OR txt NOT LIKE '%from_currency%' OR txt NOT LIKE '%to_currency%'
       OR txt NOT LIKE '%rate_type%'   OR txt NOT LIKE '%effective_date%' THEN
        RAISE EXCEPTION 'SO-39: uk_biz_exchange_rate 定义不对（期望 UNIQUE (from_currency,to_currency,rate_type,effective_date)）：%', txt;
    END IF;

    -- ===== 结构：CHECK / FK 在位 =====
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_currency'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_currency_code','ck_biz_currency_decimal_places','ck_biz_currency_status');
    IF n <> 3 THEN
        RAISE EXCEPTION 'SO-39: biz_currency CHECK 只有 % 条在位（期望 3 条）', n;
    END IF;

    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_exchange_rate'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_exchange_rate_rate','ck_biz_exchange_rate_type','ck_biz_exchange_rate_pair');
    IF n <> 3 THEN
        RAISE EXCEPTION 'SO-39: biz_exchange_rate CHECK 只有 % 条在位（期望 3 条）', n;
    END IF;

    -- CHECK 域值字面量必须齐（PK 断言按渲染形态：PG 会写成 ((rate > (0)::numeric))）
    SELECT pg_get_constraintdef(oid) INTO txt FROM pg_constraint
     WHERE conrelid = 'public.biz_exchange_rate'::regclass AND conname = 'ck_biz_exchange_rate_rate';
    IF txt NOT LIKE '%rate > (0)%' THEN
        RAISE EXCEPTION 'SO-39: ck_biz_exchange_rate_rate 定义不含 rate > 0：%', txt;
    END IF;

    SELECT pg_get_constraintdef(oid) INTO txt FROM pg_constraint
     WHERE conrelid = 'public.biz_exchange_rate'::regclass AND conname = 'ck_biz_exchange_rate_type';
    IF txt NOT LIKE '%SPOT%' OR txt NOT LIKE '%MONTH_END%' THEN
        RAISE EXCEPTION 'SO-39: ck_biz_exchange_rate_type 域值不全（必须 SPOT/MONTH_END）：%', txt;
    END IF;

    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_exchange_rate'::regclass AND contype = 'f';
    IF n <> 2 THEN
        RAISE EXCEPTION 'SO-39: biz_exchange_rate 外键 % 条（期望 2：from_currency / to_currency → biz_currency.code）', n;
    END IF;
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_exchange_rate'::regclass AND contype = 'f'
       AND pg_get_constraintdef(oid) LIKE '%REFERENCES biz_currency(code)%'
       AND pg_get_constraintdef(oid) LIKE '%ON DELETE RESTRICT%';
    IF n <> 2 THEN
        RAISE EXCEPTION 'SO-39: 两条外键必须都是 REFERENCES biz_currency(code) ON DELETE RESTRICT，实测命中 % 条', n;
    END IF;

    -- ===== 种子：4 个币种 + 本位币恰好一行且是 CNY =====
    SELECT count(*) INTO n FROM biz_currency WHERE code IN ('CNY','USD','EUR','HKD');
    IF n <> 4 THEN
        RAISE EXCEPTION 'SO-39: 4 个币种种子只到 % 行（CNY/USD/EUR/HKD）', n;
    END IF;

    SELECT count(*) INTO n FROM biz_currency WHERE is_base;
    IF n <> 1 THEN
        RAISE EXCEPTION 'SO-39: 本位币行数 = %（必须恰好 1 行）', n;
    END IF;

    SELECT code INTO txt FROM biz_currency WHERE is_base;
    IF txt IS DISTINCT FROM 'CNY' THEN
        -- 新装库（本迁移建表）一定是 CNY。能走到这里只有一种可能：有人在应用本迁移之后
        -- 手工把本位币改成了别的币种（例如把账套切到 USD）。此时**只提示不报错** ——
        -- 本位币唯一仍由索引硬保证，而「迁移不抢账套口径」优先于「必须 CNY」。
        RAISE NOTICE 'SO-39: 本位币是 %（新装库应为 CNY；非新装库视为账套已切换，仅提示）', txt;
    END IF;

    SELECT count(*) INTO n FROM biz_currency WHERE is_base AND status = 'ACTIVE';
    IF n <> 1 THEN
        RAISE EXCEPTION 'SO-39: 本位币必须是 ACTIVE 状态（实测 % 行）', n;
    END IF;

    -- ===== 行为探针 P1：第二个 is_base=true 必须被部分唯一索引拒 =====
    BEGIN
        INSERT INTO biz_currency (code, name, symbol, decimal_places, is_base, status)
        VALUES ('ZZQ', '__SO39_PROBE_SECOND_BASE__', 'Z', 2, true, 'ACTIVE');
        RAISE EXCEPTION 'SO-39: 本位币唯一索引没生效 —— 第二个 is_base=true 竟然插入成功';
    EXCEPTION WHEN unique_violation THEN
        NULL;   -- 预期路径
    END;

    -- ===== 行为探针 P2：币种码必须 ISO 三位大写 =====
    BEGIN
        INSERT INTO biz_currency (code, name, symbol, decimal_places, is_base, status)
        VALUES ('usd1', '__SO39_PROBE_BAD_CODE__', 'Z', 2, false, 'ACTIVE');
        RAISE EXCEPTION 'SO-39: 币种码 CHECK 没生效 —— 小写/超长码插入成功';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- ===== 行为探针 P3：decimal_places 越界必须被拒 =====
    BEGIN
        INSERT INTO biz_currency (code, name, symbol, decimal_places, is_base, status)
        VALUES ('ZZR', '__SO39_PROBE_DP__', 'Z', 5, false, 'ACTIVE');
        RAISE EXCEPTION 'SO-39: decimal_places CHECK 没生效 —— 5 位小数插入成功（会静默丢分）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- ===== 行为探针 P4：rate = 0 / 负数必须被拒 =====
    BEGIN
        INSERT INTO biz_exchange_rate (from_currency, to_currency, rate, rate_type, effective_date)
        VALUES ('USD', 'CNY', 0, 'SPOT', DATE '2026-01-01');
        RAISE EXCEPTION 'SO-39: rate > 0 约束没生效 —— 汇率 0 插入成功';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO biz_exchange_rate (from_currency, to_currency, rate, rate_type, effective_date)
        VALUES ('USD', 'CNY', -7.1, 'SPOT', DATE '2026-01-01');
        RAISE EXCEPTION 'SO-39: rate > 0 约束没生效 —— 负汇率插入成功';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- ===== 行为探针 P5：rate_type 域外值必须被拒 =====
    BEGIN
        INSERT INTO biz_exchange_rate (from_currency, to_currency, rate, rate_type, effective_date)
        VALUES ('USD', 'CNY', 7.1, 'AVG', DATE '2026-01-01');
        RAISE EXCEPTION 'SO-39: rate_type CHECK 没生效 —— AVG 插入成功';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- ===== 行为探针 P6：自反汇率必须被拒 =====
    BEGIN
        INSERT INTO biz_exchange_rate (from_currency, to_currency, rate, rate_type, effective_date)
        VALUES ('USD', 'USD', 1, 'SPOT', DATE '2026-01-01');
        RAISE EXCEPTION 'SO-39: from <> to CHECK 没生效 —— 自反汇率插入成功';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- ===== 行为探针 P7：不存在的币种必须被外键拒（INSERT 侧是 23503）=====
    BEGIN
        INSERT INTO biz_exchange_rate (from_currency, to_currency, rate, rate_type, effective_date)
        VALUES ('ZZX', 'CNY', 1, 'SPOT', DATE '2026-01-01');
        RAISE EXCEPTION 'SO-39: from_currency 外键没生效 —— 孤儿汇率插入成功';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO biz_exchange_rate (from_currency, to_currency, rate, rate_type, effective_date)
        VALUES ('USD', 'ZZX', 1, 'SPOT', DATE '2026-01-01');
        RAISE EXCEPTION 'SO-39: to_currency 外键没生效 —— 孤儿汇率插入成功';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;

    -- ===== 行为探针 P8：同币对同类型同生效日重复必须被唯一索引拒；换类型/换日必须放行 =====
    INSERT INTO biz_exchange_rate (from_currency, to_currency, rate, rate_type, effective_date, remark)
    VALUES ('USD', 'CNY', 7.12345678, 'SPOT', DATE '2026-01-01', '__SO39_PROBE_RATE__');
    BEGIN
        INSERT INTO biz_exchange_rate (from_currency, to_currency, rate, rate_type, effective_date)
        VALUES ('USD', 'CNY', 7.2, 'SPOT', DATE '2026-01-01');
        RAISE EXCEPTION 'SO-39: uk_biz_exchange_rate 没生效 —— 同键重复汇率插入成功';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    -- 换汇率类型：放行（同一生效日 SPOT 与 MONTH_END 是两条不同口径）
    INSERT INTO biz_exchange_rate (from_currency, to_currency, rate, rate_type, effective_date, remark)
    VALUES ('USD', 'CNY', 7.2, 'MONTH_END', DATE '2026-01-01', '__SO39_PROBE_RATE__');
    -- 换生效日：放行
    INSERT INTO biz_exchange_rate (from_currency, to_currency, rate, rate_type, effective_date, remark)
    VALUES ('USD', 'CNY', 7.05, 'SPOT', DATE '2026-02-01', '__SO39_PROBE_RATE__');
    -- 取汇率语义探针：<= 2026-01-15 的最新一条必须是 7.12345678（1 月 SPOT），不是 2 月的
    SELECT r.rate::text INTO txt FROM biz_exchange_rate r
     WHERE r.from_currency = 'USD' AND r.to_currency = 'CNY' AND r.rate_type = 'SPOT'
       AND r.effective_date <= DATE '2026-01-15'
     ORDER BY r.effective_date DESC LIMIT 1;
    IF txt <> '7.12345678' THEN
        RAISE EXCEPTION 'SO-39: 取汇率语义探针失败（期望 7.12345678，实际 %）', txt;
    END IF;

    -- ===== 探针清场：残留必须为 0 =====
    DELETE FROM biz_exchange_rate WHERE remark = '__SO39_PROBE_RATE__';
    DELETE FROM biz_currency WHERE code IN ('ZZQ','ZZR') OR name LIKE '__SO39_PROBE%';
    SELECT count(*) INTO n FROM biz_exchange_rate WHERE remark = '__SO39_PROBE_RATE__';
    IF n <> 0 THEN
        RAISE EXCEPTION 'SO-39: 汇率探针清场残留 % 行', n;
    END IF;
    SELECT count(*) INTO n FROM biz_currency WHERE code NOT IN ('CNY','USD','EUR','HKD');
    IF n <> 0 THEN
        RAISE EXCEPTION 'SO-39: 币种表出现 % 行非种子币种（探针未清场）', n;
    END IF;
    SELECT count(*) INTO n FROM biz_exchange_rate;
    IF n <> 0 THEN
        RAISE EXCEPTION 'SO-39: 汇率表上线应为空（探针残留 % 行）', n;
    END IF;

    RAISE NOTICE 'SO-39 自检通过：2 表 + 4 币种种子 + 本位币唯一 + rate>0 + 唯一索引 + 取汇率语义';
END $$;
