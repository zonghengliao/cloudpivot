-- ======================================================
-- SO-39 外部约束探针（独立于迁移内自检，用于验收留证）
--
-- 用法：PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d <库> -v ON_ERROR_STOP=1 -f 本文件
-- 设计：不出现 DROP（单查询模式危险命令栅栏）；结果落 TEMP 表，末尾汇总，
--       有 ok=false 就 RAISE EXCEPTION 让 psql 退出码 ≠ 0。
-- ======================================================
CREATE TEMP TABLE IF NOT EXISTS so39_probes (
  id     text PRIMARY KEY,
  ok     boolean NOT NULL,
  detail text
);
DELETE FROM so39_probes;

DO $$
DECLARE
    n integer;
    txt text;
BEGIN
    -- ---------- 结构 ----------
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_currency';
    INSERT INTO so39_probes VALUES ('S01 biz_currency 11 列', n = 11, n::text);

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_exchange_rate';
    INSERT INTO so39_probes VALUES ('S02 biz_exchange_rate 11 列', n = 11, n::text);

    SELECT format('%s,%s', numeric_precision, numeric_scale) INTO txt FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_exchange_rate' AND column_name='rate';
    INSERT INTO so39_probes VALUES ('S03 rate = numeric(18,8)', txt = '18,8', txt);

    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname='public' AND indexname='uk_biz_currency_single_base'
       AND indexdef LIKE 'CREATE UNIQUE INDEX%' AND indexdef LIKE '%WHERE is_base%';
    INSERT INTO so39_probes VALUES ('S04 本位币部分唯一索引在位', n = 1, n::text);

    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname='public' AND indexname='uk_biz_exchange_rate' AND indexdef LIKE 'CREATE UNIQUE INDEX%';
    INSERT INTO so39_probes VALUES ('S05 汇率四列唯一索引在位', n = 1, n::text);

    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid='public.biz_exchange_rate'::regclass AND contype='f'
       AND pg_get_constraintdef(oid) LIKE '%REFERENCES biz_currency(code)%'
       AND pg_get_constraintdef(oid) LIKE '%ON DELETE RESTRICT%';
    INSERT INTO so39_probes VALUES ('S06 两条币种外键 RESTRICT', n = 2, n::text);

    -- 迁移零 TRIGGER（本仓纪律）
    SELECT count(*) INTO n FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
     WHERE c.relname IN ('biz_currency','biz_exchange_rate') AND NOT t.tgisinternal;
    INSERT INTO so39_probes VALUES ('S07 本卡表零 TRIGGER', n = 0, n::text);

    -- ---------- 种子 ----------
    SELECT count(*) INTO n FROM biz_currency WHERE code IN ('CNY','USD','EUR','HKD');
    INSERT INTO so39_probes VALUES ('S08 4 个币种种子', n = 4, n::text);

    SELECT count(*) INTO n FROM biz_currency WHERE is_base;
    INSERT INTO so39_probes VALUES ('S09 本位币恰好 1 行', n = 1, n::text);

    SELECT code INTO txt FROM biz_currency WHERE is_base;
    INSERT INTO so39_probes VALUES ('S10 本位币 = CNY', txt = 'CNY', coalesce(txt,'<null>'));

    SELECT code || ':' || symbol INTO txt FROM biz_currency WHERE code='HKD';
    INSERT INTO so39_probes VALUES ('S11 HKD 符号 = HK$', txt = 'HKD:HK$', coalesce(txt,'<null>'));

    -- ---------- 反向探针：坏数据必须被拒 ----------
    BEGIN
        INSERT INTO biz_currency (code,name,symbol,is_base) VALUES ('ZZQ','probe-second-base','Z',true);
        INSERT INTO so39_probes VALUES ('P01 第二个本位币被拒', false, '插入成功（本位币唯一没生效）');
    EXCEPTION WHEN unique_violation THEN
        INSERT INTO so39_probes VALUES ('P01 第二个本位币被拒', true, '23505 unique_violation');
    END;

    BEGIN
        UPDATE biz_currency SET is_base = true WHERE code = 'USD';
        INSERT INTO so39_probes VALUES ('P02 UPDATE 抢本位币被拒', false, 'UPDATE 成功（约束只管 INSERT 就漏了）');
    EXCEPTION WHEN unique_violation THEN
        INSERT INTO so39_probes VALUES ('P02 UPDATE 抢本位币被拒', true, '23505 unique_violation');
    END;

    BEGIN
        INSERT INTO biz_currency (code,name,symbol) VALUES ('usd','probe-lower','Z');
        INSERT INTO so39_probes VALUES ('P03 小写币种码被拒', false, '插入成功');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO so39_probes VALUES ('P03 小写币种码被拒', true, '23514 check_violation');
    END;

    BEGIN
        INSERT INTO biz_currency (code,name,symbol,decimal_places) VALUES ('ZZR','probe-dp','Z',5);
        INSERT INTO so39_probes VALUES ('P04 decimal_places=5 被拒', false, '插入成功');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO so39_probes VALUES ('P04 decimal_places=5 被拒', true, '23514 check_violation');
    END;

    BEGIN
        INSERT INTO biz_currency (code,name,symbol,status) VALUES ('ZZT','probe-status','Z','DELETED');
        INSERT INTO so39_probes VALUES ('P05 status=DELETED 被拒', false, '插入成功');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO so39_probes VALUES ('P05 status=DELETED 被拒', true, '23514 check_violation');
    END;

    BEGIN
        INSERT INTO biz_exchange_rate (from_currency,to_currency,rate,rate_type,effective_date)
        VALUES ('USD','CNY',0,'SPOT',DATE '2026-01-01');
        INSERT INTO so39_probes VALUES ('P06 rate=0 被拒', false, '插入成功（卡面硬要求 rate>0）');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO so39_probes VALUES ('P06 rate=0 被拒', true, '23514 check_violation');
    END;

    BEGIN
        INSERT INTO biz_exchange_rate (from_currency,to_currency,rate,rate_type,effective_date)
        VALUES ('USD','CNY',-1,'SPOT',DATE '2026-01-01');
        INSERT INTO so39_probes VALUES ('P07 负汇率被拒', false, '插入成功');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO so39_probes VALUES ('P07 负汇率被拒', true, '23514 check_violation');
    END;

    BEGIN
        INSERT INTO biz_exchange_rate (from_currency,to_currency,rate,rate_type,effective_date)
        VALUES ('USD','CNY',7.1,'AVG',DATE '2026-01-01');
        INSERT INTO so39_probes VALUES ('P08 rate_type=AVG 被拒', false, '插入成功');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO so39_probes VALUES ('P08 rate_type=AVG 被拒', true, '23514 check_violation');
    END;

    BEGIN
        INSERT INTO biz_exchange_rate (from_currency,to_currency,rate,rate_type,effective_date)
        VALUES ('USD','USD',1,'SPOT',DATE '2026-01-01');
        INSERT INTO so39_probes VALUES ('P09 自反币对被拒', false, '插入成功');
    EXCEPTION WHEN check_violation THEN
        INSERT INTO so39_probes VALUES ('P09 自反币对被拒', true, '23514 check_violation');
    END;

    BEGIN
        INSERT INTO biz_exchange_rate (from_currency,to_currency,rate,rate_type) VALUES ('USD','CNY',7.1,'SPOT');
        INSERT INTO so39_probes VALUES ('P10 effective_date 无默认（必填）', false, '省略生效日竟然插入成功');
    EXCEPTION WHEN not_null_violation THEN
        INSERT INTO so39_probes VALUES ('P10 effective_date 无默认（必填）', true, '23502 not_null_violation');
    END;

    BEGIN
        INSERT INTO biz_exchange_rate (from_currency,to_currency,rate,rate_type,effective_date)
        VALUES ('ZZX','CNY',1,'SPOT',DATE '2026-01-01');
        INSERT INTO so39_probes VALUES ('P11 不存在的原币被拒', false, '插入成功（孤儿汇率）');
    EXCEPTION WHEN foreign_key_violation THEN
        INSERT INTO so39_probes VALUES ('P11 不存在的原币被拒', true, '23503 foreign_key_violation');
    END;

    -- ---------- 正向探针：合法数据必须能被接受 ----------
    INSERT INTO biz_exchange_rate (from_currency,to_currency,rate,rate_type,effective_date,remark)
    VALUES ('USD','CNY',7.12345678,'SPOT',DATE '2026-01-01','__SO39_EXT_PROBE__');

    SELECT rate::text INTO txt FROM biz_exchange_rate
     WHERE remark='__SO39_EXT_PROBE__' AND rate_type='SPOT' AND effective_date=DATE '2026-01-01';
    INSERT INTO so39_probes VALUES ('P12 8 位小数完整保留', txt = '7.12345678', coalesce(txt,'<null>'));

    -- rate_type 省略 → DEFAULT 'SPOT'
    INSERT INTO biz_exchange_rate (from_currency,to_currency,rate,effective_date,remark)
    VALUES ('EUR','CNY',7.8,DATE '2026-01-01','__SO39_EXT_PROBE__');
    SELECT rate_type INTO txt FROM biz_exchange_rate WHERE remark='__SO39_EXT_PROBE__' AND from_currency='EUR';
    INSERT INTO so39_probes VALUES ('P13 rate_type 缺省 = SPOT', txt = 'SPOT', coalesce(txt,'<null>'));

    BEGIN
        INSERT INTO biz_exchange_rate (from_currency,to_currency,rate,rate_type,effective_date)
        VALUES ('USD','CNY',7.2,'SPOT',DATE '2026-01-01');
        INSERT INTO so39_probes VALUES ('P14 同币对同类型同生效日重复被拒', false, '重复插入成功');
    EXCEPTION WHEN unique_violation THEN
        INSERT INTO so39_probes VALUES ('P14 同币对同类型同生效日重复被拒', true, '23505 unique_violation');
    END;

    -- 换类型 / 换日 必须放行（否则「按月维护」就无从谈起）
    INSERT INTO biz_exchange_rate (from_currency,to_currency,rate,rate_type,effective_date,remark)
    VALUES ('USD','CNY',7.2,'MONTH_END',DATE '2026-01-01','__SO39_EXT_PROBE__');
    INSERT INTO biz_exchange_rate (from_currency,to_currency,rate,rate_type,effective_date,remark)
    VALUES ('USD','CNY',7.05,'SPOT',DATE '2026-02-01','__SO39_EXT_PROBE__');
    SELECT count(*) INTO n FROM biz_exchange_rate WHERE remark='__SO39_EXT_PROBE__';
    INSERT INTO so39_probes VALUES ('P15 换类型/换日放行（4 行合法汇率）', n = 4, n::text);

    -- 取汇率语义：<= 2026-01-15 的最新 SPOT = 7.12345678
    SELECT rate::text INTO txt FROM biz_exchange_rate
     WHERE from_currency='USD' AND to_currency='CNY' AND rate_type='SPOT' AND effective_date <= DATE '2026-01-15'
     ORDER BY effective_date DESC LIMIT 1;
    INSERT INTO so39_probes VALUES ('P16 取 <= 某日最新汇率', txt = '7.12345678', coalesce(txt,'<null>'));

    -- ---------- 清场 ----------
    DELETE FROM biz_exchange_rate WHERE remark = '__SO39_EXT_PROBE__';
    DELETE FROM biz_currency WHERE code NOT IN ('CNY','USD','EUR','HKD');
    SELECT count(*) INTO n FROM biz_exchange_rate;
    INSERT INTO so39_probes VALUES ('C01 探针清场（汇率表回到 0 行）', n = 0, n::text);
END $$;

\echo ''
\echo '========== SO-39 外部探针结果 =========='
SELECT id, CASE WHEN ok THEN 'PASS' ELSE 'FAIL' END AS result, detail FROM so39_probes ORDER BY id;
\echo ''
SELECT count(*) FILTER (WHERE ok) || ' / ' || count(*) || ' PASS' AS summary FROM so39_probes;

-- 有失败就让 psql 退出码 ≠ 0（自动化才能当真信号）
DO $$
DECLARE
    bad integer;
    msg text;
BEGIN
    SELECT count(*), string_agg(id, '; ') INTO bad, msg FROM so39_probes WHERE NOT ok;
    IF bad > 0 THEN
        RAISE EXCEPTION 'SO-39 外部探针失败 % 条：%', bad, msg;
    END IF;
    RAISE NOTICE 'SO-39 外部探针全部通过';
END $$;
