-- P9 / B2（第三笔归因，共三笔）：手工夹具带进来的库存金额 20 元，没有任何业务单据承载
--
-- 307 元的差异是**逐笔**归因的（不是一次性抹平）：
--   ① 200 元 ← IN-20261031-000003 过账早于凭证功能上线 ⇒ 补凭证（脚本 20_d1）
--   ②  24 元 ← IN-20261031-000006 作废没退库存 ⇒ 退库存（脚本 30_e4）
--   ③  63 元 ← IN-20261031-000007 作废没退库存 ⇒ 退库存（脚本 30_e4）
--   ④  20 元 ← 本脚本：M-W1A-004 在 WH01 的 2 件 × 10 元
--   200+24+63+20 = 307 ✓（与不变式 B2 报的差额逐分对上）
--
-- ④ 的证据链（为什么判定是「无业务来源」而不是「凭证漏了」）：
--   * 该物料**没有任何收货单/采购单**（biz_stock_in 里没有它的行）；
--   * 它的来货是序列号入库 SN-20261030-000009：那条 SERIAL_IN 流水写的是
--     amount = 0、unit_cost = 0（W2b 设计上序列号入库不产生价值，价值应由采购收货凭证承载）；
--   * 之后 biz_inventory 的 amount 被改成了 20（= 2 件 × 10），来源是验收夹具直接改库
--     （流水 TRANSFER_OUT 的 before_amount 才第一次出现 20，中间没有任何单据）；
--   * 财务侧从来没有这 20 元的凭证，也不该凭空补一张 —— 补凭证等于给不存在的供应商挂账
--     （会立刻打破 B3「应付未付 = 2202 贷方余额」）。
--   ⇒ 处置：按「库存价值必须有业务单据承载」把金额归零（数量保留：货确实在库里，
--     序列号在库、流水累计也指向这 2 件），并在 docs/datafix-20261031.md 记一条待办：
--     若这批货其实有采购来源，应当补做收货单而不是给库存加钱。
--
-- 影响：归零后 C3（有库存则成本价非零）会有一条 WARN —— 这正是夹具数据的真实状态，
--     比「编一张凭证让报表好看」诚实。
--
-- 幂等：第二遍 amount 已是 0，命中 0 行。
UPDATE biz_inventory inv
   SET amount = 0,
       avg_cost = 0,
       updated_at = now()
 WHERE inv.material_id = (SELECT m.id FROM biz_material m
                           WHERE m.material_code = 'M-W1A-004' AND m.deleted_at IS NULL
                           ORDER BY m.created_at LIMIT 1)
   AND inv.warehouse_id = (SELECT w.id FROM biz_warehouse w
                            WHERE w.warehouse_code = 'WH01'
                            ORDER BY w.created_at LIMIT 1)
   AND inv.location_id IS NULL
   AND inv.qty = 2
   AND inv.amount = 20;
