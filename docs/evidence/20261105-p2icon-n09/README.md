# P2 / t_a43357af — 修 V0242 种子菜单「我的报销」图标 invoice → bill

> 卡：`t_a43357af`（父卡 `t_7f775c91` P2-TEST-ISO）
> 缺陷定性：**确定性数据缺陷**，与并发无关，也不是测试代码问题（父卡证据
> `docs/evidence/20261105-p2-test-iso/README.md` §8.1 已定性；本卡独立复现确认）。
> 结论：新增 1 条迁移 `V0252__menu_expense_icon_bill`，改 1 行 `sys_menu.icon`，
> `system::menu::` 模块 20/20 全绿。

---

## 1. 症状（原始报错）

```
---- system::menu::tests::n09_menu_icons_exist_in_tdesign stdout ----
panicked at src/system/menu/tests.rs:626:
以下菜单图标在 tdesign-icons-vue-next 中不存在（侧栏渲染空白）：
我的报销 → invoice
```

串行 / 单跑同样红（见 §6 的反证：把 icon 改回 `invoice` 后同一套库立刻复现逐字相同的报错）。

## 2. 根因

`backend/migrations/business/V0242__erp_expense_tables.up.sql:263` 种子行：

```sql
('20000000-0000-0000-0000-000000001501'::uuid, '我的报销', 2, 'business:erp:expense:list',
 '/business/erp/expense', '20000000-0000-0000-0000-000000001500'::uuid,
 'business/erp/expense/index', 10, 1, 'invoice', '我的报销（新建 / 提交 / 撤回 / 作废）', TRUE, 'business'),
```

而 `invoice` 不在 tdesign-icons-vue-next 的组件集里：

```
$ ls frontend/node_modules/tdesign-icons-vue-next/esm/components | grep -xE '(invoice|bill|receipt|file)\.js'
bill.js
file.js
```

（`invoice.js` / `receipt.js` 都不存在；`bill.js` / `bill-filled.js` 存在。）
同批 V0242 的 `money` / `wallet` / `folder` 都在图标集里，只有这一条错。

## 3. 全库普查（卡面第 2 项）

口径：干净库 = **193 条迁移全部应用**（V0251 末态，本卡迁移之前）后，
`SELECT id, name, icon FROM sys_menu WHERE deleted_at IS NULL AND icon IS NOT NULL AND icon <> ''`，
逐条对 `frontend/node_modules/tdesign-icons-vue-next/esm/components/<icon>.js` 是否存在。

```
== db=cloudpivot_p2icon 图标行数(非空且未删)=82  图标目录文件数=7068
== 不存在的图标行数=1
MISSING	invoice	我的报销	20000000-0000-0000-0000-000000001501
```

82 行 / 45 种 icon 取值里**只有这 1 行**不存在 ⇒ 修复范围就是 1 行，不做批量重映射。
原始输出：`01-selfcheck-before-V0251.txt`。修后同口径复跑：**0 条不存在**（`05-selfcheck-after-V0252.txt`）。

> 注：`sys_menu` 的 82 行里 81 行的取值都是历史 V0170（14 条不存在图标的一次性修复）之后
> 已经被校验过的名字，本卡这条是那次漏网的第 15 条。

## 4. 修法

新增 `backend/migrations/business/V0252__menu_expense_icon_bill.{up,down}.sql`
（号段核对：开工时 `ls backend/migrations/*/ | tail` 末号 = `V0251`；提交前再核对一次，仍无 `V0252`）。

- **为什么不改 V0242**：V0242 已应用到 prod 与所有既有库，`schema_migrations.checksum` 与文件内容
  绑定，改一个字节实例就 FATAL（`migration version N was modified after it was applied`）。
  沿 V0170 的口径「追加修复」。
- **取值 `bill`**：单据 / 账单，语义贴合报销单；`components/bill.js` 实测存在。
- **幂等**：`WHERE id = … AND icon = 'invoice'`，并且先 `IF v_icon IS DISTINCT FROM 'invoice' THEN NOTICE … RETURN`。
  客户自己改过图标 → 0 行受影响；菜单行被裁剪 → NOTICE 跳过，不报错。
- **自检**：更新行数必须 = 1、落库值必须 = `bill`，否则 `RAISE EXCEPTION` 让迁移失败（不静默半修）。
- **down**：改回 `invoice`（= 回到迁移前状态；该名字仍然不存在于图标集，回滚会重新引入空白图标，
  这是「严格回退」的应有语义，已在 down 文件头写明）。

## 5. 验收（4 项全过）

| # | 验收项 | 结果 | 原始输出 |
|---|---|---|---|
| 1 | 新迁移在干净库上跑通，`我的报销` 的 icon 是真实图标名 | **194 个迁移全部应用**，`schema_migrations max=252 / count=194`，icon=`bill`；升级路径（V0251→V0252）单独也验过（`应用 1 个迁移`） | `03-engine-upgrade-v251-to-v252.txt`、`04-engine-fresh-194.txt` |
| 2 | `cargo test --lib -- --exact system::menu::tests::n09_menu_icons_exist_in_tdesign` 单跑绿（不 skip / 不 ignore） | `1 passed; 0 failed; 2630 filtered out; finished in 0.22s`，且打印 `[N09] 全部菜单图标均真实存在 ✓`（真跑，不是 SKIP） | `06-n09-single-run.txt` |
| 3 | `cargo test --lib system::menu::` 模块 20/20 全绿 | `test result: ok. 20 passed; 0 failed; 0 ignored` | `08-menu-module-20of20.txt` |
| 4 | 迁移号不与他人撞；只按路径提交 | 迁移号 `V0252`（提交前复核磁盘仍无他人占用）；commit 只含 `backend/migrations/business/V0252__*` + 本证据目录 | `git show --stat HEAD` |

建库姿势：`RECREATE=1 ERP_DB=cloudpivot_p2icon_fresh PORT=18153 bash scripts/verify/erp_clean_db.sh`
（脚本内用 `createdb -O alun` + 起一次实例让引擎应用 `migrations/` 全部迁移 + bootstrap）。

测试环境（隔离树，避免共享工作区里别的卡的在途改动）：

```
REPO   = /root/p2icon-src          # git archive HEAD(ee78eae) + 本卡 2 个迁移文件 + node_modules 软链 + config/*.toml
TARGET = /root/menu-iso-target     # 父卡暖缓存
SYS_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_p2icon_fresh
```

## 6. 反证：n09 仍是「活防线」

把 icon 临时改回 `invoice` → n09 立刻红，报错与卡面逐字相同；改回 `bill` → 绿：
见 `07-n09-negative-control.txt`。说明这条用例的判据没有被我放松，修复是**数据侧**的。

```
### 1) 把「我的报销」icon 改回 invoice（模拟修复前状态）
UPDATE 1 ; icon = invoice
### 2) 跑 n09（期望 FAILED）
以下菜单图标在 tdesign-icons-vue-next 中不存在（侧栏渲染空白）：
我的报销 → invoice
test result: FAILED. 0 passed; 1 failed
### 3) 复原为 bill
UPDATE 1 ; icon = bill
```

## 7. 待办 / 未做的事

- 未改测试代码（`n09` 是常驻防线，卡面明确要求不动）。
- 未改 `V0242`（已应用迁移不可改）。
- 生产（144）本卡**未部署**：`/opt/cloudpivot` 的迁移会在下次发版起实例时自动应用
  （`应用 1 个迁移`）；本卡只交付迁移文件 + 证据。
