-- A4 验收用数据：给「字典项导出」准备 10 万行真实数据
--
-- 为什么用 sys_dict_item 而不是造一张测试表：
--   ① 导出定义是**真实注册**的 builtin 定义（system.dict_item），走的是产品的真实取数 SQL
--      （JOIN + 关键词过滤 + keyset 分页），不是为测试特写的假定义；
--   ② 这条链路同时验证了 dict 翻译、keyset 分页、列快照；
--   ③ 造的数据全部挂在独立字典 a4_bench 下、值前缀 A4T-，事后按前缀即可精确清理
--      （基线 sys_dict_item = 71 行，验收完要回到这个数）。
--
-- 用法：su - postgres -c "psql -d cloudpivot_test -f /root/a4_seed_100k.sql"

\set ON_ERROR_STOP on

INSERT INTO sys_dict_type (id, dict_code, dict_name, description, status)
VALUES (gen_random_uuid(), 'a4_bench', 'A4 导出压测字典', 'A4 验收专用，可整条删除', 1)
ON CONFLICT DO NOTHING;

INSERT INTO sys_dict_item (id, dict_type_id, item_value, item_label, description, sort_order, status)
SELECT gen_random_uuid(),
       t.id,
       'A4T-' || lpad(g::text, 6, '0'),
       'A4 压测项 ' || g,
       'A4 benchmark row #' || g,
       g,
       1
FROM generate_series(1, 100000) AS g
CROSS JOIN (SELECT id FROM sys_dict_type WHERE dict_code = 'a4_bench' AND deleted_at IS NULL) AS t
WHERE NOT EXISTS (
    SELECT 1 FROM sys_dict_item i
    WHERE i.dict_type_id = t.id AND i.item_value = 'A4T-' || lpad(g::text, 6, '0')
);

SELECT (SELECT count(*) FROM sys_dict_item) AS total_dict_item,
       (SELECT count(*) FROM sys_dict_item i JOIN sys_dict_type t ON t.id = i.dict_type_id
         WHERE t.dict_code = 'a4_bench') AS a4_bench_rows;
