# SO-39 证据包 —— 币种 + 汇率表（迁移 V0272）

卡面：`【SO-39】币种 + 汇率表 + 迁移`（ARCH-7 §15.6.3）
交付形态：**纯存储层**（两张主数据表 + 约束 + 索引 + 4 币种种子 + 迁移自检），无 Rust 代码、无 API、无前端。

## 1. 结论（逐条对验收）

| 验收项 | 结果 | 证据 |
|---|---|---|
| 迁移 APPLY_OK | ✅ 引擎真实应用：`数据库迁移完成: 应用 245 个迁移`（原 244 + 本卡 1），`schema_migrations` 记账 `272 \| versioned \| executed \| t` | `migration_probe.txt` ①②③ |
| 4 个币种种子 | ✅ `CNY / USD / EUR / HKD` 四行，CNY `is_base=t`、其余 `f`，均 `ACTIVE`、`decimal_places=2` | `migration_probe.txt` ⑤、`constraint_probes.txt` S08–S11 |
| 本位币唯一约束生效 | ✅ 部分唯一索引 `uk_biz_currency_single_base`：INSERT 第二个 `is_base=true` → 23505；UPDATE 抢本位币 → 23505 | `constraint_probes.txt` P01 / P02 |
| rate 必须 > 0 | ✅ `ck_biz_exchange_rate_rate`：0 与负数均 23514 | `constraint_probes.txt` P06 / P07 |
| 唯一索引 uk_biz_exchange_rate | ✅ `(from_currency, to_currency, rate_type, effective_date)` 四列 UNIQUE；重复被拒、换类型/换日放行 | `migration_probe.txt` ⑥、`constraint_probes.txt` P14 / P15 |
| 迁移 kind / applied_via 口径 | ✅ `versioned` / `executed` | `migration_probe.txt` ③ |
| 回滚可逆 + 重放幂等 | ✅ down → 两表与索引全消失；up → 全回来且探针仍 28/28；再 up → 幂等 | `rollback_drill.txt` |
| 已迁移库重启不重放 | ✅ `应用 0 个迁移` + `/health` 200 | `boot_health.txt` |

外部独立探针 **28 / 28 PASS**（`constraint_probes.txt`）。

## 2. 交付物

| 文件 | 内容 |
|---|---|
| `backend/migrations/business/V0272__sales_currency.up.sql` | `biz_currency`(11 列) + `biz_exchange_rate`(11 列) + 2 个唯一索引 + 3+3 条 CHECK + 2 条 FK + 4 币种种子 + 末尾 DO 块自检（结构 / 种子 / 8 组行为探针 / 清场残留=0） |
| `backend/migrations/business/V0272__sales_currency.down.sql` | 逆序删（先引用方 `biz_exchange_rate`，再被引用方 `biz_currency`）+ 行数提示 + 反向自检 |
| `docs/evidence/20261107-so39-currency/constraint-probes.sql` | 外部独立探针（不复制迁移内逻辑，自清场，末尾汇总，有 FAIL 就 RAISE） |
| `scripts/verify/so39_rollback_drill.sh` | 克隆库 down→up→第三轮探针→幂等重放→清场 |
| `scripts/verify/so39_boot_health.sh` | 已迁移库再起实例：`应用 0 个迁移` + `/health` 200 + 记账行核对 |

## 3. 关键口径（下游 SO-40 必须照抄）

1. **汇率方向**：`1 from_currency = rate to_currency`，只存「外币 → 本位币」；反向用 `1 / rate` 推导，**不落重复行**（两行互为倒数必然因四舍五入对不上）。
2. **取汇率 SQL**（生效日含当日，取最新一条）：
   ```sql
   SELECT rate FROM biz_exchange_rate
    WHERE from_currency = $1 AND to_currency = $2 AND rate_type = $3
      AND effective_date <= $4
    ORDER BY effective_date DESC LIMIT 1;
   ```
   `uk_biz_exchange_rate` 的最左前缀正好覆盖这条查询，不需要额外索引。
3. **汇率锁定**：单据创建时把 `rate` 拷进单据自己的列（SO-40 的 `exchange_rate`），后续汇率变动不改已建单据（ARCH §15.6.4 ①）。
4. **精度**：`rate NUMERIC(18,8)`、单据金额 `NUMERIC(18,4)`；币种 `decimal_places` 限 0..4，>4 会在「原币 × 汇率 = 本位币」链路上静默丢分。
5. **本位币唯一**只能在库层保证一条：写入 `is_base=true` 前必须先把旧本位币置 false（两步在同一事务内），否则撞 23505。
6. **不做软删**：币种停用走 `status='INACTIVE'`；`biz_exchange_rate` 两条外键都是 `ON DELETE RESTRICT`，被引用的币种删不掉。

## 4. 决策与偏离说明

| 项 | ARCH 稿面 | 本卡落点 | 理由 |
|---|---|---|---|
| 迁移号 | 卡面 `V0272` | `V0272`（未被占用，最大号 V0313） | 乱序补迁被 alun-db 允许（仅 WARN）；先例 V0270。开工与提交前各 `ls` 复核过 |
| `to_currency` 外键 | 稿面只给 `from_currency` 标 `FK →` | 两条 FK 都建（`REFERENCES biz_currency(code) ON DELETE RESTRICT`） | 否则能录出「兑不存在币种」的孤儿汇率，月末重估静默跳过它 |
| `effective_date` 默认值 | 稿面「必填」 | `NOT NULL`，**不给 DEFAULT** | 给 `CURRENT_DATE` 会让补录历史汇率静默按今天生效 → 月末重估错月 |
| `rate_type` 可空性 | 稿面未写 | `NOT NULL DEFAULT 'SPOT'` | 唯一索引成员；PG 里 NULL 彼此不相等，可空会让同键重复插入成功 |
| `decimal_places` 上限 | 稿面「默认 2」 | `CHECK BETWEEN 0 AND 4` | 本仓金额列统一 `NUMERIC(18,4)` |
| 自反汇率 | 稿面未写 | `CHECK (from_currency <> to_currency)` | 自反汇率恒 1，落库只会让「取最新一条」多噪声行 |
| 种子本位币写法 | 稿面「CNY(base)」 | `is_base = v.is_base AND NOT EXISTS(已有本位币)` | 写死 `true` 会在「库中已另立本位币（USD）且 CNY 行缺失」时撞部分唯一索引、迁移直接失败；现算后两条路径都不抛错 |
| 自检里的本位币断言 | —— | `count(is_base)=1` + 本位币必须 ACTIVE 是硬断言；「本位币≠CNY」仅 `NOTICE` | 新装库必然是 CNY（引擎 APPLY 证据已证）；非新装库视为账套已切换，迁移不抢口径 |
| 软删列 | 稿面未提 | 不加 `deleted_at` | 4~10 行极小主数据，停用由 `status` 表达；软删会与部分唯一索引/外键组合出空洞 |
| Rust 代码 | 卡面只要求表 + 迁移 | 不新增 | 与 SO-32（价格两表）同范式：配置/主数据卡只落存储层；取汇率与汇率锁定属 SO-40 |

## 5. 复现命令

```bash
# ① 空库全链迁移（APPLY_OK）
ERP_DB=cloudpivot_so39_clean PORT=18072 RECREATE=1 bash scripts/verify/erp_clean_db.sh
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so39_clean \
  -Atc "select version,kind,applied_via,success from schema_migrations where version='272'"

# ② 外部探针（28 项）
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so39_clean \
  -v ON_ERROR_STOP=1 -f docs/evidence/20261107-so39-currency/constraint-probes.sql

# ③ 回滚演练（克隆库，秒级）
bash scripts/verify/so39_rollback_drill.sh cloudpivot_so39_clean

# ④ 已迁移库重启不重放
bash scripts/verify/so39_boot_health.sh cloudpivot_so39_clean
```

## 6. 残留 / 给下游

- `cloudpivot_so39`（克隆 V0313 的迭代库）与 `cloudpivot_so39_clean`（空库全链）保留在本机 99，实例均已停；`_drill` 库演练后已删。
- 汇率维护接口（CRUD + 按日取汇率 API）与双币字段不在本卡：见 SO-40。
- 前端币种/汇率页面未做（本卡无前端改动，`pnpm typecheck && pnpm build` 无需触发）。

## 7. 边界场景演练：库中已另立本位币时的重放

在迭代库 `cloudpivot_so39` 上构造「USD 已是本位币 + CNY 行被删」，再重放 `up.sql`：

```
UPDATE biz_currency SET is_base=false; UPDATE biz_currency SET is_base=true WHERE code='USD';
DELETE FROM biz_currency WHERE code='CNY';
-- 重放 up.sql
NOTICE:  SO-39: 本位币是 USD（新装库应为 CNY；非新装库视为账套已切换，仅提示）
NOTICE:  SO-39 自检通过：2 表 + 4 币种种子 + 本位币唯一 + rate>0 + 唯一索引 + 取汇率语义
-- 结果：CNY|false  EUR|false  HKD|false  USD|true     ← 不抛错、不抢口径、CNY 以非本位币补录
```

对照：第一版种子把 CNY 的 `is_base` 写死 `true`，同一场景会直接撞 `uk_biz_currency_single_base`（23505）导致迁移失败 —— 已修。

