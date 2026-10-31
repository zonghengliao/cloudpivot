-- P9 / D1 回滚：删掉 20_d1 补出来的凭证与应付单，并清空收货单上的两个引用
--
-- 号段（biz_seq 的 PZ / AP 的 current_no）**不回退**：回退会与线上已发出的号撞车，
-- 宁可留一个号段空洞（凭证号不连续不影响任何不变量）。
DO $$
DECLARE
    v_ids uuid[];
    r_id  uuid;
BEGIN
    SELECT id INTO r_id FROM biz_stock_in WHERE in_no = 'IN-20261031-000003' AND deleted_at IS NULL;
    IF r_id IS NULL THEN
        RAISE NOTICE 'P9-D1 down: 单据不存在，跳过';
        RETURN;
    END IF;

    SELECT coalesce(array_agg(v.id), '{}'::uuid[]) INTO v_ids
      FROM biz_voucher v
     WHERE v.source_id = r_id
       AND v.remark LIKE 'P9 数据修复%';

    IF array_length(v_ids, 1) IS NULL THEN
        RAISE NOTICE 'P9-D1 down: 没有本脚本补的凭证，跳过（幂等）';
        RETURN;
    END IF;

    DELETE FROM biz_voucher_entry WHERE voucher_id = ANY (v_ids);
    DELETE FROM biz_payable WHERE source_id = r_id AND remark LIKE 'P9 数据修复%';
    DELETE FROM biz_voucher WHERE id = ANY (v_ids);
    UPDATE biz_stock_in SET voucher_id = NULL, payable_id = NULL, updated_at = now() WHERE id = r_id;

    RAISE NOTICE 'P9-D1 down: 已删除凭证 % 与对应的应付单', v_ids;
END $$;
