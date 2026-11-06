# SO-01 客户档案表 + 迁移（t_aed20a56）· 验收证据

日期：2026-11-06 · 机器：`172.18.10.99:/opt/cloudpivot`（工作仓）· 隔离库：`cloudpivot_v0260`
迁移：`backend/migrations/business/V0260__sales_customer.{up,down}.sql`
验收脚本：`scripts/verify/v0260_customer_check.sh`

## 0. 迁移号确认（开工实测）

```
$ ls backend/migrations/business | tail -4
V0258__business_role_matrix.down.sql
V0258__business_role_matrix.up.sql
V0259__flow_instance_button_parentage.down.sql
V0259__flow_instance_button_parentage.up.sql
```
→ 磁盘最高 V0259，本卡取 **V0260**（提交前再复核）。

## 1. APPLY_OK（引擎真实执行，不是手工 psql）

干净库由 `scripts/verify/erp_clean_db.sh RECREATE=1` 建（owner=alun）+ 起一次 `target/debug/cloudpivot`：

```
==> created: cloudpivot_v0260 (owner=alun)
2026-11-06T01:03:08.202699Z  INFO nik_web::app: 数据库迁移完成: 应用 202 个迁移
    schema_migrations(max|count) = 260|202
```

`schema_migrations` 记账行（硬要求：kind=versioned，applied_via 只能是 executed/imported）：

```
-[ RECORD 1 ]-------------------------------------------------
kind        | versioned
name        | sales_customer
version     | 260
applied_via | executed
success     | t
source      | migrations/business/V0260__sales_customer.up.sql
ck_len      | 64
```

引擎记账指纹 == 文件 sha256：

```
db  = fd7107861edfe0baa730feeb191ef987234c8935eff09d4b1e837d22d7c4536f
file= fd7107861edfe0baa730feeb191ef987234c8935eff09d4b1e837d22d7c4536f
MATCH ✓
```

## 2. 表结构（psql 直读，节选；完整见 round-*.txt）

索引：

```
biz_customer         | biz_customer_pkey                 | CREATE UNIQUE INDEX ... (id)
biz_customer         | idx_biz_customer_name             | CREATE INDEX ... (name)
biz_customer         | idx_biz_customer_status           | CREATE INDEX ... (status)
biz_customer         | uk_biz_customer_code              | CREATE UNIQUE INDEX ... (code) WHERE (deleted_at IS NULL)
biz_customer_contact | idx_biz_customer_contact_customer | CREATE INDEX ... (customer_id)
biz_customer_contact | uk_biz_customer_contact_primary   | CREATE UNIQUE INDEX ... (customer_id) WHERE ((is_primary = 1) AND (deleted_at IS NULL))
```

约束：

```
biz_customer         | ck_biz_customer_credit_limit      | CHECK ((credit_limit >= (0)::numeric))
biz_customer         | ck_biz_customer_status            | CHECK (((status)::text = ANY (ARRAY['ACTIVE'::character varying, 'FROZEN'::character varying])))
biz_customer_contact | ck_biz_customer_contact_primary   | CHECK ((is_primary = ANY (ARRAY[0, 1])))
biz_customer_contact | fk_biz_customer_contact_customer  | FOREIGN KEY (customer_id) REFERENCES biz_customer(id) ON DELETE RESTRICT
```

关键列默认值：`biz_customer.status default 'ACTIVE'::character varying`、`credit_limit default 0`、
`is_primary default 0`、四件审计列 + `deleted_at`（两张表都有）。

## 3. 种子（3 个客户 + 各 1 个主联系人）

```
CUS-0001 | 北京云枢科技有限公司 | ACTIVE | 500000.0000 | 主联系人=张伟
CUS-0002 | 上海云枢贸易有限公司 | ACTIVE | 200000.0000 | 主联系人=李娜
CUS-0003 | 广州云枢实业有限公司 | ACTIVE |     0.0000 | 主联系人=王强
[PASS] 客户种子条数 = 3
[PASS] 主联系人条数 = 3
[PASS] 孤儿联系人 = 0
[PASS] 探针残留 = 0
```

## 4. 回滚脚本可执行（down → 空 → up → 复原）

```
=== DOWN 回放 ===
DROP TABLE
DROP TABLE
rc=0
=== DOWN 后状态 ===
(已删) / (已删) / 记账行=1        ← 表没了、schema_migrations 行仍在（引擎语义：手工回放不改账）
=== UP 回放（回滚后重建）===
INSERT 0 3
INSERT 0 3
DO
=== UP 再跑一遍（幂等）===
INSERT 0 0
INSERT 0 0
DO                                ← 幂等：不重复种、不覆盖客户改动
=== 引擎重启（同库）===
2026-11-06T01:03:38.921576Z  INFO nik_web::app: 数据库迁移完成: 应用 0 个迁移
                                  ← 手工 down/up 往返后指纹仍一致，引擎零待应用
```

## 5. 连跑两轮验收

`round-1.txt` / `round-2.txt`：两轮都 **15 项 [PASS] / 0 项 [FAIL]，rc=0**。

## 6. 顺带跑的常驻闸

```
$ python3 scripts/verify/role_code_gate.py
✓ 无**新增/改动**的未知角色码（历史基线全部按指纹豁免）   rc=0
```
（本迁移不写 `sys_role_menu`，与闸⑧无关；记录在此仅为「提交前检查一遍」的凭据。）
