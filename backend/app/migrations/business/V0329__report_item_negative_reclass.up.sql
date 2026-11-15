-- ======================================================
-- V0329：资产负债表「留抵税额」重分类（FIX-6）
--
-- 现象：进项 260 > 销项 52 ⇒ `2221 应交税费` 期末是**借方余额** 208。
--       报表按「负债类科目贷方为正」取数，于是「应交税费」显示 **-208**。
--       会计口径上这不是负债为负，而是**留抵税额**（可结转下期抵扣），应重分类到
--       资产侧「其他流动资产」列报。
--
-- 做法（配置驱动，不写死科目编码到代码里）：
--   ① `biz_report_item` 加一列 `reclass_negative_to`（弱引用同报表的 `item_code`）：
--      「本项算出的金额为负时，把 -金额 搬到目标项、自己按 0 列示」；
--   ② 把 `TAXES_PAYABLE`（应交税费）的该列指向 `OTHER_CURRENT_ASSETS`（其他流动资产）；
--   ③ 给目标项补一句 remark，说明这行里可能含着「待抵扣进项税额」。
--
-- 为什么不直接在 `report.rs` 里写死「2221 → 1901」：报表结构本来就是库里的配置
-- （`biz_report_item` 可由「报表项管理」页维护），写死等于把口径分裂成两份。
--
-- 幂等：`ADD COLUMN IF NOT EXISTS` + `WHERE ... IS DISTINCT FROM`（第二遍更新 0 行）。
-- 回滚：down 只删本迁移加的列（结构回到 V0183 + 之后各次的形状，取值随之消失）。
-- ======================================================

ALTER TABLE biz_report_item
    ADD COLUMN IF NOT EXISTS reclass_negative_to VARCHAR(32);

COMMENT ON COLUMN biz_report_item.reclass_negative_to IS
    '负数重分类目标项编码（FIX-6）：本项算出的金额为负时，把 -金额 搬到该 item_code、自己按 0 列示；'
    '典型场景是「应交税费」期末借方余额（留抵税额）重分类到「其他流动资产」';

UPDATE biz_report_item
   SET reclass_negative_to = 'OTHER_CURRENT_ASSETS'
 WHERE report_code = 'BALANCE_SHEET'
   AND item_code = 'TAXES_PAYABLE'
   AND reclass_negative_to IS DISTINCT FROM 'OTHER_CURRENT_ASSETS';

UPDATE biz_report_item
   SET remark = coalesce(nullif(btrim(remark), ''), '其他流动资产')
                || '；FIX-6：留抵税额（应交税费期末借方余额）重分类到本行，列报为「待抵扣进项税额」'
 WHERE report_code = 'BALANCE_SHEET'
   AND item_code = 'OTHER_CURRENT_ASSETS'
   AND coalesce(remark, '') NOT LIKE '%留抵税额%';

-- ---------- 自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    target      TEXT;
    target_hit  INT;
    same_report INT;
BEGIN
    SELECT reclass_negative_to INTO target
      FROM biz_report_item
     WHERE report_code = 'BALANCE_SHEET' AND item_code = 'TAXES_PAYABLE';
    IF target IS DISTINCT FROM 'OTHER_CURRENT_ASSETS' THEN
        RAISE EXCEPTION 'V0329 自检失败：应交税费的重分类目标没配上（实际 %）',
            coalesce(target, '<null>');
    END IF;

    -- 目标项必须同报表存在，否则重分类后会「搬给一个不存在的行」→ 报表少钱
    SELECT count(*) INTO target_hit
      FROM biz_report_item
     WHERE report_code = 'BALANCE_SHEET' AND item_code = target;
    IF target_hit <> 1 THEN
        RAISE EXCEPTION 'V0329 自检失败：重分类目标 % 在 BALANCE_SHEET 里不存在', target;
    END IF;

    -- 目标项必须挂在资产侧，否则「负数搬到负债侧」等于没修
    SELECT count(*) INTO same_report
      FROM biz_report_item
     WHERE report_code = 'BALANCE_SHEET' AND item_code = target AND side = 'ASSET';
    IF same_report <> 1 THEN
        RAISE EXCEPTION 'V0329 自检失败：重分类目标 % 不在资产侧（side 必须是 ASSET）', target;
    END IF;

    -- 目标项必须被「资产总计」的算式覆盖，否则搬过去也不进合计 → 报表不平
    IF NOT EXISTS (
        SELECT 1 FROM biz_report_item
         WHERE report_code = 'BALANCE_SHEET'
           AND item_code = 'CURRENT_ASSETS_TOTAL'
           AND formula LIKE '%OTHER_CURRENT_ASSETS%'
    ) THEN
        RAISE EXCEPTION 'V0329 自检失败：CURRENT_ASSETS_TOTAL 的算式没包含 OTHER_CURRENT_ASSETS';
    END IF;
END
$$;
