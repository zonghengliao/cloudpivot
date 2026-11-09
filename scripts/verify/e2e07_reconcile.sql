-- E2E-07 合同域勾稽（145 cloudpivot_dev）
-- 输出格式：check|<实际值>|<期望值>，由 e2e07_run_all.sh 逐行比对。
-- 口径：只统计本卡自造数据（contract_name / 通知 biz_id 均以 E2E07- 合同为锚），不碰别人的单。
\pset tuples_only on
\pset format unaligned
\pset fieldsep '|'

-- ① 合同当前状态 = 最后一条状态日志的 to_status（状态机与日志不脱节）
SELECT 'check',
       coalesce((
         SELECT count(*) FROM biz_contract c
          WHERE c.deleted_at IS NULL AND c.contract_name LIKE 'E2E07-%'
            AND c.status <> (SELECT l.to_status FROM biz_status_log l
                              WHERE l.entity_type = 'CONTRACT' AND l.entity_id = c.id
                              ORDER BY l.created_at DESC, l.id DESC LIMIT 1)
       ), -1)::text,
       '0';

-- ② 状态日志里出现的「非法迁移」：同合同 to_status 连续重复 / from 与上一条 to 不一致
SELECT 'check',
       coalesce((
         WITH logs AS (
           SELECT entity_id, from_status, to_status,
                  lag(to_status) OVER (PARTITION BY entity_id ORDER BY created_at, id) AS prev_to
             FROM biz_status_log
            WHERE entity_type = 'CONTRACT'
              AND entity_id IN (SELECT id FROM biz_contract WHERE contract_name LIKE 'E2E07-%')
         )
         SELECT count(*) FROM logs WHERE prev_to IS NOT NULL AND prev_to <> from_status
       ), -1)::text,
       '0';

-- ③ 联动通知幂等键重复（同一合同同一状态发了两条）
SELECT 'check',
       coalesce((
         SELECT count(*) FROM (
           SELECT idempotency_key FROM sys_notice
            WHERE biz_type = 'CONTRACT' AND idempotency_key LIKE 'LNK:CONTRACT:%'
              AND biz_id IN (SELECT id::text FROM biz_contract WHERE contract_name LIKE 'E2E07-%')
            GROUP BY idempotency_key HAVING count(*) > 1
         ) t
       ), -1)::text,
       '0';

-- ④ 采购单只能挂在 PURCHASE 合同上（类型闸没有漏）
SELECT 'check',
       coalesce((
         SELECT count(*) FROM biz_purchase_order o
           JOIN biz_contract c ON c.id = o.contract_id
          WHERE o.deleted_at IS NULL AND c.contract_name LIKE 'E2E07-%'
            AND c.contract_type <> 'PURCHASE'
       ), -1)::text,
       '0';

-- ⑤ 销售单只能挂在 SALES 合同上，且客户 = 合同乙方
SELECT 'check',
       coalesce((
         SELECT count(*) FROM biz_sales_order o
           JOIN biz_contract c ON c.id = o.contract_id
          WHERE o.deleted_at IS NULL AND c.contract_name LIKE 'E2E07-%'
            AND (c.contract_type <> 'SALES' OR o.customer_id <> c.party_b_id)
       ), -1)::text,
       '0';

-- ⑥ 任何本卡合同不允许「已执行金额 > 合同金额」（超签）
SELECT 'check',
       coalesce((
         SELECT count(*) FROM (
           SELECT c.id, c.amount,
                  coalesce((SELECT sum(o.total_with_tax) FROM biz_purchase_order o
                             WHERE o.contract_id = c.id AND o.status <> 'CANCELLED' AND o.deleted_at IS NULL), 0)
                + coalesce((SELECT sum(o.total_with_tax) FROM biz_sales_order o
                             WHERE o.contract_id = c.id AND o.status <> 'CANCELLED' AND o.deleted_at IS NULL), 0) AS committed
             FROM biz_contract c
            WHERE c.deleted_at IS NULL AND c.contract_name LIKE 'E2E07-%'
         ) t WHERE committed > amount + 0.005
       ), -1)::text,
       '0';

-- ⑦ 服务 / 租赁合同不允许挂任何订单
SELECT 'check',
       coalesce((
         SELECT count(*) FROM biz_contract c
          WHERE c.deleted_at IS NULL AND c.contract_type IN ('SERVICE', 'LEASE')
            AND (EXISTS (SELECT 1 FROM biz_purchase_order o WHERE o.contract_id = c.id AND o.deleted_at IS NULL)
              OR EXISTS (SELECT 1 FROM biz_sales_order o WHERE o.contract_id = c.id AND o.deleted_at IS NULL))
       ), -1)::text,
       '0';

-- ⑧ 到期提醒台账：同一合同同一档位只一条
SELECT 'check',
       coalesce((
         SELECT count(*) FROM (
           SELECT doc_id, stage FROM biz_reminder_log
            WHERE doc_type = 'CONTRACT'
              AND doc_id IN (SELECT id FROM biz_contract WHERE contract_name LIKE 'E2E07-%')
            GROUP BY doc_id, stage HAVING count(*) > 1
         ) t
       ), -1)::text,
       '0';

-- ⑨ 变更留痕只发生在提交审批之前（审批中 / 已签不应再有变更日志）
SELECT 'check',
       coalesce((
         SELECT count(*) FROM sys_change_log l
           JOIN biz_contract c ON c.id::text = l.resource_id
          WHERE l.resource = 'biz_contract' AND c.contract_name LIKE 'E2E07-%'
            AND EXISTS (SELECT 1 FROM biz_status_log s
                         WHERE s.entity_id = c.id AND s.to_status = 'PENDING_APPROVAL'
                           AND l.created_at > s.created_at)
       ), -1)::text,
       '0';

-- ⑩ 合同链路不记账：本卡合同号不作为凭证来源
SELECT 'check',
       coalesce((
         SELECT count(*) FROM biz_voucher v
          WHERE v.source_no IN (SELECT contract_no FROM biz_contract WHERE contract_name LIKE 'E2E07-%')
       ), -1)::text,
       '0';

-- ⑪ 合同附件：扫描件上传人不留空、不落裸 UUID
SELECT 'check',
       coalesce((
         SELECT count(*) FROM biz_doc_attachment a
          WHERE a.doc_type = 'CONTRACT'
            AND a.doc_id IN (SELECT id FROM biz_contract WHERE contract_name LIKE 'E2E07-%')
            AND a.file_role = 'CONTRACT_SCAN'
            AND (coalesce(a.uploader_name, '') = '' OR a.uploader_name ~ '^[0-9a-f]{8}-')
       ), -1)::text,
       '0';

-- ⑫ 采购单表头 = 明细行合计（金额自算不被篡改）
SELECT 'check',
       coalesce((
         SELECT count(*) FROM biz_purchase_order o
          WHERE o.deleted_at IS NULL
            AND o.contract_id IN (SELECT id FROM biz_contract WHERE contract_name LIKE 'E2E07-%')
            AND abs(o.total_with_tax - coalesce((SELECT sum(i.with_tax_amount) FROM biz_purchase_order_item i
                                                  WHERE i.order_id = o.id), 0)) > 0.005
       ), -1)::text,
       '0';

-- ⑬ 采购单上没有「非合同乙方」的供应商（供应商从合同派生）
SELECT 'check',
       coalesce((
         SELECT count(*) FROM biz_purchase_order o
           JOIN biz_contract c ON c.id = o.contract_id
          WHERE o.deleted_at IS NULL AND c.contract_name LIKE 'E2E07-%'
            AND o.supplier_id <> c.party_b_id
       ), -1)::text,
       '0';
