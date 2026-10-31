-- ============================================================================
-- P14 / 20：冲销 M-W1A-004 / WH01 的 10 件零成本库存，并写一条冲销流水
-- ----------------------------------------------------------------------------
-- 目标：`biz_inventory` 该组 qty=10 / amount=0 / avg_cost=0  →  qty=0 / amount=0 / avg_cost=0
--       并写一条 `OUT` 冲销流水（qty=10，金额 0），让 B4「流水累计 = 当前库存」继续成立。
--
-- 为什么要写流水（而不是直接改 qty）：
--   `erp_invariants.py` 的 B4 = 「Σ(IN) − Σ(OUT) = 当前库存」。这 10 件当初就是靠
--   一条 `IN / SERIAL_IN / SERIAL_BATCH / SN-20261031-000011` 记进来的；只把 qty 抹成 0
--   会让 B4 报「流水累计 10 ≠ 库存 0」—— 从一条 BLOCKER（B1）换成另一条 BLOCKER。
--   写冲销流水才是账本的正确做法：原始流水一行不动（审计不改历史），冲销单独成行。
--
-- 字段取值的三个坑（已按**库里真实约束**核对过，不是照卡面猜的）：
--   ① `ck_biz_inv_txn_qty CHECK (qty > 0)` —— 出库数量必须写**正数 10**，写 -10 直接违约。
--   ② `ck_biz_inv_txn_amt CHECK (amount >= 0)` —— 金额 0 合法（这正是「零成本」的形态）。
--   ③ `biz_inventory_txn.biz_type` **没有** CHECK 白名单（全库只有 txn_type 有
--      CHECK IN ('IN','OUT')），所以 `PRODUCE_CANCEL` 可以落库，不需要加迁移放宽。
--      取值说明：库里既有的冲销约定是「原 biz_type + _CANCEL」（`PURCHASE_IN_CANCEL`）。
--      本批原始流水是 `SERIAL_IN`，按字面约定该叫 `SERIAL_IN_CANCEL`；卡面点名
--      `PRODUCE_CANCEL`（对应序列号 source_type='PRODUCE'），两者都不违约，这里**按卡面**取
--      `PRODUCE_CANCEL` —— 将来若有人给 biz_type 加白名单迁移，必须把这两个值一起放进去。
--   ④ `doc_id` 是 NOT NULL：这里写**固定的** uuid（同时用固定主键 id），
--      ⇒ 幂等靠 `on conflict (id) do nothing`，回滚靠固定 id 精确删除，不依赖 doc_no 模糊匹配。
--
-- 不产生凭证（金额 0 → 无财务影响）：
--   这 10 件 amount = 0.0000 ⇒ 当初就没有生成凭证（`biz_voucher` 里 source_no 含
--   SN-20261031-000011 的行数 = 0，见 30_verify.sql ⑥）⇒ 清理同样不需要红冲。
--   本脚本**不写任何 biz_voucher / biz_voucher_entry**。
--
-- 库位账：该组本来 0 行（批量生成只写成本账，不写库位账 —— 这正是 B1 红的根因）。
--   清完仍是 0 行，与成本账 0 一致 ⇒ B1 转绿。脚本里两条断言把它钉住（前 0 行、后一致）。
--
-- 幂等：成本账已归零且冲销流水已存在 → NOTICE 跳过（第二遍 0 行受影响）。
-- 前置校验（任一不符即 RAISE EXCEPTION，整条脚本回滚）：
--   成本账该组恰好 1 行；qty = 10；amount = 0；avg_cost = 0；库位账合计 = 0。
-- ============================================================================
do $$
declare
    c_batch  constant text := 'P14';
    c_doc_no constant text := 'DATAFIX-20261101-P14';
    c_txn_id constant uuid := '01a1f140-0000-4000-8000-000000000001';
    c_doc_id constant uuid := '01a1f140-0000-4000-8000-000000000000';
    c_qty    constant numeric := 10;
    v_mat   uuid;
    v_wh    uuid;
    v_rows  int;
    v_inv   biz_inventory%rowtype;
    v_loc   numeric;
    v_txn   int;
    v_snapped int;
    v_n     int;
    v_cum   numeric;
begin
    select id into v_mat from biz_material
     where material_code = 'M-W1A-004' and deleted_at is null
     order by created_at limit 1;
    if v_mat is null then
        raise exception 'P14/20: 找不到物料 M-W1A-004 —— 停下人工核对';
    end if;
    select id into v_wh from biz_warehouse where warehouse_code = 'WH01';
    if v_wh is null then
        raise exception 'P14/20: 找不到仓库 WH01 —— 停下人工核对';
    end if;

    -- ---------- ① 锁行 + 必须恰好 1 行 ----------
    select count(*) into v_rows
      from biz_inventory where material_id = v_mat and warehouse_id = v_wh;
    if v_rows <> 1 then
        raise exception 'P14/20: M-W1A-004/WH01 成本账有 % 行（预期恰好 1 行）—— 停下人工核对', v_rows;
    end if;

    select * into v_inv
      from biz_inventory
     where material_id = v_mat and warehouse_id = v_wh
       for update;

    -- ---------- ② 幂等判定 ----------
    select count(*) into v_txn from biz_inventory_txn where id = c_txn_id;
    if v_txn = 1 then
        -- 固定 id 已被占用：必须就是我们那条，否则停
        select count(*) into v_txn
          from biz_inventory_txn where id = c_txn_id and doc_no = c_doc_no and txn_type = 'OUT';
        if v_txn <> 1 then
            raise exception 'P14/20: uuid % 已被非本批流水占用（doc_no 不是 %）—— 停下人工核对', c_txn_id, c_doc_no;
        end if;
        if v_inv.qty = 0 and v_inv.amount = 0 and v_inv.avg_cost = 0 then
            raise notice 'P14/20: 成本账已归零且冲销流水已存在 —— 幂等跳过（本步 0 行受影响）';
            return;
        end if;
        raise exception 'P14/20: 冲销流水已存在但成本账仍是 qty=% / amount=% / avg_cost=% —— 半截状态，停下人工核对',
            v_inv.qty, v_inv.amount, v_inv.avg_cost;
    end if;

    -- ---------- ③ 前置校验：必须是清理前的那一组值 ----------
    if v_inv.qty <> c_qty then
        raise exception 'P14/20: 成本账 qty = %（预期 10）—— 情况已变，停下人工核对，不自动冲销', v_inv.qty;
    end if;
    if v_inv.amount <> 0 then
        raise exception 'P14/20: 成本账 amount = %（预期 0）—— 有金额就要走凭证红冲，本脚本不处理，停下', v_inv.amount;
    end if;
    if v_inv.avg_cost <> 0 then
        raise exception 'P14/20: 成本账 avg_cost = %（预期 0）—— 停下人工核对', v_inv.avg_cost;
    end if;

    -- ---------- ④ 库位账必须是 0（本来就是 0；有行说明已被人补过，语义变了） ----------
    select coalesce(sum(qty), 0) into v_loc
      from biz_inventory_location where material_id = v_mat and warehouse_id = v_wh;
    if v_loc <> 0 then
        raise exception 'P14/20: 库位账合计 = %（预期 0）—— 与「批量生成只写成本账」的事实不符，停下人工核对', v_loc;
    end if;

    -- ---------- ⑤ 整行快照（down 精确写回用） ----------
    insert into datafix_p14_snapshot (batch, tbl, pk, row_data)
    values (c_batch, 'biz_inventory', v_inv.id, to_jsonb(v_inv))
    on conflict (batch, tbl, pk) do nothing;
    get diagnostics v_snapped = row_count;

    -- ---------- ⑥ 冲销成本账 ----------
    update biz_inventory
       set qty        = v_inv.qty - c_qty,
           amount     = v_inv.amount,          -- 金额本来就是 0，减 0 不动
           avg_cost   = case when v_inv.qty - c_qty = 0 then 0 else v_inv.avg_cost end,
           last_txn_at = now(),
           updated_at  = now()
     where id = v_inv.id;
    get diagnostics v_n = row_count;
    if v_n <> 1 then
        raise exception 'P14/20: 成本账更新 % 行（预期 1）—— 已回滚', v_n;
    end if;

    -- ---------- ⑦ 写冲销流水（before/after 六字段真填） ----------
    insert into biz_inventory_txn (
        id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
        qty, unit_cost, amount,
        before_qty, before_amount, before_cost,
        after_qty,  after_amount,  after_cost,
        period_code, operator_id, remark, created_at
    ) values (
        c_txn_id, v_mat, v_wh, 'OUT', 'PRODUCE_CANCEL', 'ADJUST', c_doc_id, c_doc_no, 1,
        c_qty, 0, 0,
        v_inv.qty, v_inv.amount, v_inv.avg_cost,
        0, 0, 0,
        '2026-10', null,
        'P14 数据修复：清理 2026-10-31 16:17:24 老板测试产生的零成本批量生成数据（M-W1A-004/WH01 10 件，unit_cost=0，无凭证）',
        now()
    );

    -- ---------- ⑧ 后置断言 ----------
    select * into v_inv from biz_inventory where id = v_inv.id;
    if v_inv.qty <> 0 or v_inv.amount <> 0 or v_inv.avg_cost <> 0 then
        raise exception 'P14/20: 冲销后成本账不归零（qty=% / amount=% / avg_cost=%）—— 已回滚',
            v_inv.qty, v_inv.amount, v_inv.avg_cost;
    end if;

    -- 流水累计（IN 加、OUT 减）必须等于当前库存，否则 B4 会红
    select coalesce(sum(case when txn_type = 'IN' then qty else -qty end), 0) into v_cum
      from biz_inventory_txn where material_id = v_mat and warehouse_id = v_wh;
    if v_cum <> v_inv.qty then
        raise exception 'P14/20: 冲销后流水累计 % ≠ 库存 % —— 已回滚', v_cum, v_inv.qty;
    end if;

    raise notice 'P14/20: 成本账已归零 + 冲销流水已写（doc_no=% / qty=% / 金额 0 / 快照新增 % 行）',
        c_doc_no, c_qty, v_snapped;
    raise notice 'P14/20: 库位账保持 0 行，成本账 0 ⇒ B1 应转绿；流水累计 % = 库存 %', v_cum, v_inv.qty;
end
$$;
