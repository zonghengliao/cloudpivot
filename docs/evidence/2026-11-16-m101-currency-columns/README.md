# M1-01 证据：多币种存储层第一步（V0406）

> 卡：`t_76e55ffd` —— 迁移 `biz_account` 加 `currency_code` + `biz_voucher` 加
> `currency_code` / `exchange_rate`（两表加列 + 回填 CNY + 自检块）。
> 依据：`docs/planning/20261113-新卡清单.md` §4 M1 财务 的 M1-01 行。

## 改了什么

| 文件 | 改动 |
|------|------|
| `backend/app/migrations/business/V0406__account_voucher_currency.up.sql`（新增） | `biz_account.currency_code VARCHAR(8) NOT NULL DEFAULT 'CNY'`；`biz_voucher.currency_code VARCHAR(8) NOT NULL DEFAULT 'CNY'` + `exchange_rate NUMERIC(18,8) NOT NULL DEFAULT 1`；3 条 CHECK、2 条索引、存量回填、4 个自检块（前置护栏 / 约束守卫 / 回填 / 结构+行为探针） |
| `backend/app/migrations/business/V0406__account_voucher_currency.down.sql`（新增） | 幂等回滚（`DROP … IF EXISTS`）+ **有外币数据时拒绝回滚**的护栏 + 删完自检 |
| `backend/app/tests/m1_01_currency_columns_migration.rs`（新增，10 条静态不变量） | 列数/类型/精度/默认值、3 条 CHECK、2 条索引、回填带 `IS NULL` 谓词、只碰两张表、自检块必须有 `RAISE EXCEPTION`、down 幂等与护栏、引擎拆分器逐块验；纯文件解析不连库 |
| `scripts/verify/m101_currency_columns_drill.sh`（新增） | 真库往返演练脚本（克隆模板库 → up×2 → 结构断言 → down×2 → 重放 up → down 护栏反证 → 自删演练库） |

**没有**改 Rust 业务代码、前端、`vendor/nik-*`，也没有改任何已应用的迁移文件。

## 五个刻意的决定

| 决定 | 为什么不选另一种 |
|------|------|
| 列名取 **`currency_code`**（卡面写的是 `currency`） | 全仓 12 处同类列（`biz_contract` / `biz_payable` / `biz_sales_order` / `biz_receivable` / `biz_receipt` / `biz_invoice` / `biz_purchase_order` …）一律 `currency_code`，且 M1-05 调汇、M1-09 报表折算都要 join `biz_currency.code`。取名 `currency` 会让本仓出现两套列名，写错时只在运行期报 `column does not exist`（列名不参与类型检查） |
| 币种列 **`VARCHAR(8)`** 不是 `CHAR(3)` | `CHAR(3)` 在 UNION / JOIN 带定长空格语义，与 `biz_currency.code VARCHAR(8)` 比较要隐式转换；V0314 已把 `biz_receivable.currency_code` 从 `CHAR(3)` 放宽过，不许倒退 |
| 汇率 **`NUMERIC(18,8)`**（与 `biz_exchange_rate.rate` / V0314 同精度） | 写窄（如 18,6）会让 JPY→CNY 这类 0.04x 量级汇率丢有效位，月末调汇的折算差额变成「找不到来源的尾差」 |
| 收紧成 **无条件** `exchange_rate > 0` | CNY 行 rate 恒 1，`1 > 0` 天然满足 ⇒ 无条件收紧没有副作用；而 rate = 0 会让折算后本位币金额全 0（账面金额凭空消失），是本卡唯一「会静默算错账」的入口 |
| **不加**外键指向 `biz_currency(code)` | 与 V0314 销售链四表同口径（主数据弱引用，硬外键只建在 `biz_exchange_rate` 的币种对上）；加了会让币种主数据变成凭证表的删除依赖，而「币种必须存在」由录入入口保证即可 |

## 验证结果

### 1) 真库往返（145 的 PG，模板库 `cloudpivot_test` 克隆；全量输出见 `01_drill.out`）

```
bash scripts/verify/m101_currency_columns_drill.sh
== 汇总：PASS=22 FAIL=0 ==
```

覆盖：up 首次成功（自检块通过）→ up 重复执行幂等 → 三列类型/精度（`varchar(8)` / `18,8`）/
`NOT NULL` / 默认值（`'CNY'` / `1`）→ 3 条 CHECK 在位 → 2 条索引在位（凭证索引是
`WHERE currency_code <> 'CNY'` 的部分索引）→ 存量 **83 行** `biz_account` 全部回填 `CNY`、
无空值 → 三条约束**行为反证**（`usd` / 汇率 0 / 两位币种全部被拒）→ down 首次成功 →
down 重复执行幂等 → 回滚后重放 up 成功 → **down 护栏反证**（造一条 USD 科目后 down
被拒且结构不动）。

### 2) 克隆库升级（**真引擎**，不是 psql 手动执行；全量输出见 `02_engine_upgrade.out`）

模板库 `cloudpivot_test`（versioned 326 条，最大 V0404）克隆成 `cp_m101_upgrade`，
用 `backend/target/debug/cloudpivot` 真启动一次（profile=test，PG 指 145）：

| 检查 | 结果 |
|------|------|
| 首次启动迁移日志 | `数据库迁移完成: 应用 2 个迁移`（V0405 + 本卡 V0406） |
| `schema_migrations` | `versioned / account_voucher_currency / 406 / ffd620623f8f… / executed / t`，`source = app/migrations/business/V0406__account_voucher_currency.up.sql` |
| 升级后结构 | 三列到位（`character varying(8)` / `numeric(18,8)`，均 `NOT NULL`，默认 `'CNY'` / `1`） |
| 存量回填 | `biz_account` 83 行全部 `CNY` |
| 重启一次 | `数据库迁移完成: 应用 0 个迁移`（引擎级幂等） |
| `/health` / `/health/deps` | 200 / `{"status":"ok","db":{"ok":true},"cache":{"ok":true},…}` |

> 为什么这一步不能省：psql 直跑只能证明 **SQL 是对的**，证明不了
> 「加载器认得这个文件名 / 版本号没重号 / 指纹写进了 `schema_migrations`」。
> 这三件事任何一件错，都是**上线那一刻服务起不来**。

### 3) 静态不变量（`cargo test`，不连库）

```
cargo test --test m1_01_currency_columns_migration        # 10 条：列/精度/默认值/CHECK/索引/回填谓词/范围/自检/拆分器
cargo test --test flow_all_role_multiversion_migration    # 7 条：全局版本号唯一 / 4 位补零 / up 必有 down
```

### 4) 门禁

```
cd backend && cargo fmt -- --check && cargo clippy --lib -- -D warnings
```
