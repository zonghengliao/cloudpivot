# 数据修复与清场 SOP（手工修数 / 演示数据清理，10 条）

> 生效：2026-11-04 · 卡：`t_3efbe252` · 上游决定：`docs/architecture/P2-6-生产首跑命中收口决定.md`（D-1 / D-2 / D-3）
> 适用：**任何**直接改生产库数据的手工修数、清场、演示数据清理、批量生成事故善后。
> 不适用：正常业务功能内的单据作废/红冲（走业务接口）；巡检代码本身的改动（见卡 A / `docs/architecture/快照一致性巡检与告警设计.md`）。
>
> **固定顺序（不许跳步）**：只读复现 → 行级快照 → 修数（成对/幂等）→ 对平验证 → 巡检复跑 → 留档 + 豁免。
> 本文每条都能照着敲；出现「看情况」的字眼就是本文的 bug，请提 PR 改它。

---

## 1. 先快照后动手：`datafix_<批次>_snapshot`，形状固定，别发明新格式

库里已有三张同形状先例（2026-11-04 生产实测）：`datafix_p14_snapshot`（batch=`P14`，10+1 行）、
`datafix_flowclean_snapshot`（batch=`FLOWCLEAN-20261101`，9 张表 149 行）、`datafix_e2e_residue_snapshot`（batch=`E2ERES-20261104`，8 张表 961 行）。

```sql
-- ① 建表（一次批次一张表，形状照抄，pk 一律用 text：p14 那张是历史 uuid，别学）
CREATE TABLE IF NOT EXISTS datafix_<批次小写>_snapshot (
  batch       text        NOT NULL,
  tbl         text        NOT NULL,
  pk          text        NOT NULL,
  row_data    jsonb       NOT NULL,
  captured_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (batch, tbl, pk)
);

-- ② 存受影响行（在**同一事务**里、先于任何 DML 执行）
INSERT INTO datafix_<批次小写>_snapshot (batch, tbl, pk, row_data)
SELECT '<批次号>', 'biz_material_serial', id::text, to_jsonb(s)
  FROM biz_material_serial s
 WHERE <与后面 DML 完全相同的 WHERE 条件>
ON CONFLICT DO NOTHING;

-- ③ 硬断言：快照行数必须等于目标行数，不等就停
DO $$
DECLARE snap int; tgt int;
BEGIN
  SELECT count(*) INTO snap FROM datafix_<批次小写>_snapshot WHERE batch='<批次号>' AND tbl='biz_material_serial';
  SELECT count(*) INTO tgt  FROM biz_material_serial WHERE <同一条件>;
  IF snap <> tgt THEN RAISE EXCEPTION '快照 % 行 ≠ 目标 % 行，停止修数', snap, tgt; END IF;
END $$;
```

红线：**先改后补快照 = 违规**（补出来的不是"修数前"，是"修数后"）。目标行数在动手时要点出来（`NULL` 的列也要进快照，`to_jsonb` 天然带全列）。

## 2. 先只读复现，再写入：三条断言，一条都别省

修数前用**只读会话**跑一遍，把判据和行数钉死（本文 §1 的 `tgt` 就是这里数出来的）：

```bash
# 只读会话（PG 层面拒绝写语句，比"我小心点"可靠）
PGOPTIONS='-c default_transaction_read_only=on' PGPASSWORD=*** psql -h 127.0.0.1 -U cloudpivot -d cloudpivot -X -f 修数前检查.sql
```

```sql
-- ① 命中行数（严进：多一条也不许）
SELECT count(*) FROM biz_material_serial WHERE <判据>;
-- ② 形状复核（判据里没写死的属性，逐项用它兜底；实测与预期不符就停）
SELECT source_type, status, warehouse_id, count(*) FROM biz_material_serial WHERE <判据> GROUP BY 1,2,3;
-- ③ 外部引用检查（有引用 ⇒ 说明进过别的单据，必须改走单据作废，不许直接改数据）
SELECT count(*) FROM biz_stock_transfer_item_serial WHERE serial_id IN (SELECT id FROM biz_material_serial WHERE <判据>);
```

先例可抄：`scripts/datafix/p14/apply.sh` 的四条件 + 形状复核 + 无引用 + 硬行数（10/0 两态，其它值抛异常）。

## 3. 账务类修数必须**成对**：有原流水就必须有对冲流水

库存/成本/往来一类的修数，**永远不要 UPDATE 掉数量或金额**（会把一条 BLOCKER 换成另一条：流水累计 ≠ 库存）。写一条反向流水，原流水一行不动：

```sql
INSERT INTO biz_inventory_txn (
  id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
  qty, unit_cost, amount, before_qty, before_amount, before_cost,
  after_qty, after_amount, after_cost, period_code, operator_id, remark
) VALUES (
  '<固定 uuid，末尾 …-000000000001>',                -- 固定主键 ⇒ 幂等 + 可精确回滚
  '<物料 id>', '<仓库 id>',
  'OUT',                                            -- 方向由 txn_type 表达（ck: IN/OUT）
  '<原 biz_type>_CANCEL',                           -- 既有约定：原 biz_type + _CANCEL
  'ADJUST',                                         -- 手工修数专用 doc_type（无实体表，见 §8）
  '<批次哨兵 uuid，末尾 …-000000000000>',            -- doc_id NOT NULL，见 §8
  '<批次号>',                                        -- doc_no 必须带批次号，便于 grep 全库
  1,
  <正数 qty>,                                        -- ck_biz_inv_txn_qty CHECK (qty > 0)：不许写负数
  <unit_cost>, <amount>,                             -- ck_biz_inv_txn_amt CHECK (amount >= 0)
  <before_qty>, <before_amount>, <before_cost>,
  <after_qty>, <after_amount>, <after_cost>,
  '<原期间如 2026-10>',                               -- 冲销归原会计期间，别跨期
  NULL,                                             -- operator_id 留空：不伪造操作人，身份由 doc_no+remark 表达
  '<为什么修、修了什么、原值多少，一句话写清>'
);
```

硬约束（生产 `pg_constraint` 实测，2026-11-04）：`txn_type ∈ {IN,OUT}`、`qty > 0`、`amount >= 0`、`doc_id NOT NULL`。

## 4. 对平验证：链首尾相接 + 累计 = 库存 + 目标值归零

修数后**必须**跑这三条，任一不过就不算修完：

```sql
-- ① 链是否首尾相接（后一条 before 必须等于前一条 after）
SELECT id, created_at, before_qty, after_qty,
       before_qty - lag(after_qty) OVER (ORDER BY created_at, id) AS chain_break
  FROM biz_inventory_txn WHERE material_id = '<物料 id>' ORDER BY created_at, id;
-- ② 流水累计 = 当前库存
SELECT sum(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) AS txn_net,
       (SELECT qty FROM biz_inventory WHERE material_id='<物料 id>' AND warehouse_id='<仓库 id>') AS inv_qty
  FROM biz_inventory_txn WHERE material_id='<物料 id>';
-- ③ 目标账面是否归零/符合预期（数量/金额/成本三件一起看）
SELECT qty, amount, avg_cost, locked_qty FROM biz_inventory WHERE material_id='<物料 id>' AND warehouse_id='<仓库 id>';
```

先例实测（P14 生产 + 本地探针库各跑一遍）：`chain_break` **首行为 NULL、其后每行必须是 `0.0000`**（≠0 就是断链，停手查）；
`txn_net 0.0000` = `inv_qty 0.0000`；`qty 0.0000 / amount 0.0000 / avg_cost 0.000000`（`docs/evidence/datafix/DATAFIX-20261101-P14.md` §2.2–2.3）。

## 5. 修数收尾**必跑巡检**：新增命中要么修干净，要么按卡 A 的台账登记

```bash
cd /opt/cloudpivot && set -a && . /etc/cloudpivot/cloudpivot.env && set +a
# 只跑多态引用组（DI-17…DI-25），JSON 落盘留档；退出码 0=无 BLOCKER
python3 scripts/verify/snapshot_audit.py --group 多态引用 --json --out /tmp/audit-after.json --log-file /tmp/audit-after.log
# 或直接查视图看单条等级
#   SELECT * FROM di.c_di18; SELECT * FROM di.c_di19;
```

新增命中只有两条合法出路（**不许第三种**）：

| 出路 | 怎么做 |
|---|---|
| a. 修干净 | 回到 §3/§4 把引用/悬空消除，重跑巡检直到该条消失 |
| b. 登记「已核查豁免」 | 改 `docs/architecture/snapshot-drift/10_checks.sql` 里的 `di.v_poly_ref_waiver`（**视图 + VALUES，不是表**），提交 git 后 `python3 scripts/verify/snapshot_audit.py --install-views --group 多态引用` 生效 |

豁免的四条硬要求（卡 A 口径，缺一不可）：
`check_id` + `src_table` + `src_id` **精确三元组**（禁止按 `doc_type` / 表名 / 前缀 / 通配豁免）、`reason`（为什么可以不管）、
`checked_by` + `checked_on`（谁在哪天核的）、`expires_on ≤ checked_on + 90 天`（**到期 JOIN 失效自动回 ERROR**，靠 `expires_on > current_date` 写进 JOIN 条件，不靠人记得删）。
**不许**在卡里/评论里口头说「已知」了事——巡检不认口头。

## 6. 清场/演示数据清理：**不连带软删审批实例**，报告必须附巡检前后对比

- 审批实例（`biz_flow_instance` / `biz_flow_task`）**不随业务对象删除而删**：任务删除只终止 `PENDING` 的那一张（`task_bridge.rs`「任务已删除，审批单自动终止」），
  终态实例是审计证据（与 T12 / R-P1-1 口径一致）。清场时动业务对象、**不动终态实例**。
- 清理报告必须贴**巡检前后对比**（不是"我跑过了"）：

```bash
# 清场前
python3 scripts/verify/snapshot_audit.py --group 多态引用 --json --out /tmp/audit-before.json --log-file /tmp/audit-before.log
# …执行清理…
python3 scripts/verify/snapshot_audit.py --group 多态引用 --json --out /tmp/audit-after.json  --log-file /tmp/audit-after.log
# 对比汇总（BLOCKER/WARN/INFO 各多少，逐个 check 的 hit 增减）
python3 -c "import json,sys
for f in sys.argv[1:]:
    d=json.load(open(f)); s=d['summary']
    print(f, 'BLOCKER=%d WARN=%d INFO=%d' % (s['blocker'], s['warn'], s['info']))
    print('  ', {c['id']: c['hit'] for c in d['checks'] if c['hit']})" /tmp/audit-before.json /tmp/audit-after.json
```

## 7. 保留实体一律**不删不改**：审计链只做对冲与留档

| 表 | 规则 |
|---|---|
| `biz_inventory_txn` | 一行不删不改（原流水是审计证据）；修数靠**追加**对冲流水（§3） |
| `biz_voucher` / `biz_voucher_entry` | 不删不改；金额为 0 的修数无凭证可红冲，**不要为了"配对"而新造凭证** |
| `biz_status_log` | 只增不改；实体被删是正常生命周期，日志必须留着（DI-20 = INFO，见 D-3） |
| `biz_flow_instance` 终态 | 不删不改（DI-19 终态 = INFO，见 D-2） |

需要"这笔数据不再有效"的语义时，用**软删 + 反向流水 + 留档**表达，不用物理删除。

## 8. 假 id / 假实体禁止：没实体表的类型不许编一个 uuid

生产实测（2026-11-04，`information_schema.columns`）：`biz_inventory_txn.doc_id` 是 **`NOT NULL` uuid**，`doc_type` 也 `NOT NULL`。
所以「ADJUST/OPENING 这类没有实体表的 doc_type 把 `doc_id` 留 NULL」**目前不可执行**（会被约束拒绝）。可执行的规则是：

1. `doc_type` 有实体表的（`STOCK_IN` / `STOCK_OUT` / `STOCK_TRANSFER` / `SERIAL_BATCH` / `PURCHASE_ORDER` …）：
   `doc_id` **必须是真实单据 id**，并能在 `di.v_poly_ref_map` 里查到对应目标表 —— 填"看得像单据、其实不存在"的 uuid 是**真缺陷**（DI-18 会红）。
2. `doc_type` 本来没有实体表的（`ADJUST` / `OPENING`）：
   `doc_id` 只准用**批次哨兵** `'<本批流水 id 前缀>-0000-4000-8000-000000000000'`（P14 先例：`01a1f140-0000-4000-8000-000000000001` ↔ `…-000000000000`），
   且该 `doc_type` 必须在 `di.v_poly_ref_map` 里登记为 **无目标表**（`target_table` 为空）⇒ 巡检按 `NO_TARGET_TABLE` 出 INFO，不再当悬空。
3. 真要让 `doc_id` 能留 NULL，必须先**单独提迁移**（`ALTER TABLE biz_inventory_txn ALTER COLUMN doc_id DROP NOT NULL` + 同步巡检口径 + 前端展示兜底），
   **不许在修数事务里顺手改 schema**。
4. 不许为了"让来源栏有值"而写假实体（例：为凑 `source_id` 往 `biz_status_log` 插一条 `entity_type=SERIAL_BATCH` 的假日志）——
   该类型未在 `di.v_poly_ref_map` 登记，会当场触发 **DI-22 登记缺口**，用假实体换假日志，还多一条命中。

## 9. 幂等 + 可回滚 + 先在副本库演练

1. **固定主键**：修数用的流水/记录一律写死 uuid（P14：`…000000000001`），重放时 `ON CONFLICT DO NOTHING` / 命中已有行则跳过；
   禁止用 `doc_no LIKE 'DATAFIX%'` 做模糊匹配再删（会误伤同批其它行）。
2. **down 脚本与 up 一一对应**，down 按快照精确写回（`datafix_<批次>_snapshot.row_data`）；
   保留快照表，**确认不再需要回滚后**才 `DROP TABLE datafix_<批次>_snapshot;`。
3. **先在 pg_dump 副本库演练**：`up → up（幂等）→ down → up`，四步全绿才允许碰生产。先例 `scripts/datafix/p14/drill.sh`（实测 `FAIL=0`）。
4. 生产执行前必须有 `pg_dump` 备份 + `MANIFEST.txt`（表名 / 行数 / md5），先例 `/root/datafix-backup-p14-20261101-002030/`。

## 10. 留档：五件套缺一不可

| # | 产物 | 位置 / 形态 |
|---|---|---|
| ① | 行级快照 | `datafix_<批次>_snapshot`（§1） |
| ② | 对冲流水 | `biz_inventory_txn`，`doc_no=批次号`、`remark` 写原因（§3） |
| ③ | **人读台账** | `docs/evidence/datafix/<批次号>.md`：批次、起因、被改对象、对冲流水、快照、修后账面、结论，**每条数字附可复现 SQL + 原始输出**（样板：`DATAFIX-20261101-P14.md`） |
| ④ | 脚本级留痕 | `scripts/datafix/<批次>/`（up/down/verify/apply/drill）+ 执行日志与备份 MANIFEST |
| ⑤ | 巡检结果 + 豁免 | 修数后 `--group 多态引用 --json` 的 JSON/日志；若登记豁免，同时写进 `di.v_poly_ref_waiver` 并提交 git（§5） |

**报告模板（照抄这 6 行）**：
`批次 / 起因 / 动了哪些行（表 + 行数 + 判据）/ 对冲流水 id + 对平结果 / 巡检前后对比（BLOCKER-WARN-INFO）/ 备份路径 + 回滚命令`。

---

## 附：与其它口径的边界（避免互相踩）

| 口径 | 出处 | 与本 SOP 的关系 |
|---|---|---|
| DI-18 已核查豁免（精确三元组 + 90 天到期） | 卡 A `t_237f1caa` / `di.v_poly_ref_waiver` | §5 的 b 出路就是它；本 SOP 不重复定义豁免格式，只要求「必须登记」 |
| DI-19 按实例状态分级（PENDING=WARN / 终态=INFO） | 卡 A / 决定 D-2 | §6「不连带软删审批实例」的分级依据；清场后 DI-19 只会**增加 INFO**，不产生 WARN |
| DI-20 状态日志 INFO、不处置 | 决定 D-3 | §7 保留 `biz_status_log` 的理由；清理报告里 DI-20 条数增加属正常 |
| 展示层容错（「关联业务」按 `biz_type` 路由 + 已清场置灰） | 卡 E `t_3e59cb01` | 清场后前端**允许**出现"对象已清场，仅可查看审批快照"，这不是数据缺陷；**不要**为消除置灰而恢复数据 |
| cron 接线（先降噪再挂） | 决定 D-4 / 卡 C | §5 要求先跑出 BLOCKER 0，才谈得上挂 `0 2 * * *` |

---

## 附二：本文命令的可执行性验证（2026-11-04，写 SOP 时逐条跑过）

| 本文位置 | 怎么验的 | 结果 |
|---|---|---|
| §1 快照表 DDL + `INSERT … to_jsonb` + 断言 DO 块 | 本地探针库（dev `cloudpivot_3b`，临时 schema，整段 `BEGIN … ROLLBACK`，不留痕） | `NOTICE 断言通过：快照 3 行 = 目标 3 行`；`snapshot_rows=3` |
| §3 对冲流水 INSERT 模板 | 同上（原 IN + 对冲 OUT 各插一条） | 两条均写入成功；约束层面 `qty` 正数、`amount>=0`、`doc_id NOT NULL` 全部通过 |
| §3/§8 的约束事实 | 同上，故意插负数 `qty` 与 `doc_id NULL` | `qty>0` 约束拒绝负数；`doc_id` `NOT NULL` 拒绝 NULL ⇒ 证明「留 NULL」当前不可执行、对冲必须 OUT+正数 |
| §4 三条对平验证 | ① 生产只读（P14 那两条流水）② 本地探针库 | 生产：`chain_break` 首行 NULL、次行 `0.0000`；`txn_net 0.0000 = inv_qty 0.0000`；探针库同样结果 |
| §5 巡检命令 | 生产 144 实跑 `--group 多态引用 --json` | `BLOCKER 0 / WARN 0 / INFO 80`，退出码 0（DI-18=2、DI-19=47、DI-20=31，全部 INFO） |
| §6 前后对比一行命令 | 在生产 144 用两份 JSON 实跑 | 正确打印 `BLOCKER=… WARN=… INFO=…` 与逐项 hit |
| §8 的 `doc_id NOT NULL` 结论 | 生产 `information_schema.columns` 只读查询 | `doc_id` `is_nullable = NO` |
