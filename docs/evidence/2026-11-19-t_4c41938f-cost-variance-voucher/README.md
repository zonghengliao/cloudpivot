# M5-16 差异凭证（`t_4c41938f`）交付证据

一句话：**把 M5-15 的差异分摊结果变成一张能落库的凭证** —— 差异腿逐成本对象成行（借 / 贷按金额符号翻转），
损益腿按方向汇总两行，经 `voucher::post_voucher`（全仓唯一凭证写入口）落库；一个会计期间一张，
重复生成 409 + `66112`。零迁移、零路由、零权限码、无前端改动。

| 项 | 值 |
|---|---|
| 新增模块 | `backend/app/src/business/erp/cost/cost_variance_voucher.rs`（纯逻辑 + 落库缝） |
| 新增真库用例 | `backend/app/src/business/erp/cost/cost_variance_voucher_db_tests.rs`（`ERP_DB_URL` 开关） |
| 新错误码 | `ERP_COST_VARIANCE_VOUCHER_DUPLICATE = 66112`（成本段续号，`66111` 之后） |
| 改动文件 | `cost/mod.rs`（两行模块声明）、`cloudpivot-kernel/.../error_code.rs`（一个常量 + 文档） |
| 迁移 | **无**（`biz_voucher` / `biz_voucher_entry` / `biz_accounting_period` / `1404` · `5401` 科目全部已存在） |
| 路由 / 权限码 / 菜单 | **零新增**（凭证是期末结账 / 结算事务的内部步骤，设计 §5.2 无「发差异凭证」接口） |
| 前端 | N/A（纯后端） |

## 1. 口径（为什么这么记）

```
不利差异（alloc_amount > 0）  借 5401 主营业务成本     贷 1404 材料成本差异
有利差异（alloc_amount < 0）  借 1404 材料成本差异     贷 5401 主营业务成本
```

* **金额**：一律取 `VarianceAllocLine::alloc_amount`（已含 M5-15 的尾差 `rounding_adj`），
  本模块**不二次取整**、也**不用 `share_rate` 算钱**（M5-15 单测已证用它反算会差 0.0001）。
* **方向**：取该行金额的**符号**（正 = 不利 / 负 = 有利），0 元行不生成分录
  （`participates=false` 的零基数行留在分摊结果里，只是不进凭证）。
* **科目**：ARCH-9 §4.9.3 的 D-15 原是「差异不入账」，本卡（M5-16）要出凭证，差异必须落科目。
  约束三条：不碰 `4001.01/.02/.03`（巡检 C-COST-3 判 4001 余额 = Σ期末在制 + 在途）、
  不碰 `4101.*`（C-COST-1 / C-COST-4 判池清零）、不碰 `1405`（C-COST-2 判完工凭证借方 = 入库金额）。
  剩下能用的只有「差异类 + 损益类」→ 差异腿 `1404 材料成本差异`（设计给未来 ADR 留的科目，
  V0183:76 已存在），对方腿 `5401 主营业务成本`。
* **已知妥协（报备，不是 bug）**：本系统存货按实际成本计价，`1404` 采购端没有借方，所以本凭证会让
  1404 出现与差异等额的**贷方余额**（报表层与 1405 合并看即实际成本）；人工 / 制造费用差异也挂 1404
  （它是全仓唯一的差异类科目）。科目映射已抽成 `VarianceAccountPolicy`（按要素三字段 + 对方科目），
  ADR 落地时只改 `DEFAULT_VARIANCE_ACCOUNTS` 一张表，算数与分录结构不动
  （单测 `six_types_map_into_the_default_policy` 把当前口径钉死，改口径会立刻红）。
* **已归属额不重复入账**：`attributed_amount`（已挂 `cost_object_id` 的差异行）不进这张凭证 ——
  那部分差异自己就带着对象，由落表方按行出；本凭证只发**已分摊部分**（= `allocatable_amount`）。

## 2. 硬断言（破了即 500，不是用户数据问题）

1. 期间级闭合：`Σ已归属 + Σ已摊 = Σ差异合计`（复用 M5-15 `closes_exactly()`）；
2. 单池闭合：同上，池级（每个池单独校验，报错点名类型 + 物料）；
3. 守恒：`Σ分录带符号金额 = Σ池待分摊额`；
4. 借贷平衡：`Σ借 = Σ贷`（差异腿 + 损益腿）；
5. 三合计自洽：`不利 + 有利 = 净额`。

## 3. 真跑输出（本目录 `raw/`，全部真执行）

| 文件 | 内容 | 结果 |
|---|---|---|
| `raw/01-db-tests.txt` | 145 真库 `cp_m516_drill`（`-T cp_m511_test` 克隆，397 条迁移）：5 条用例 | **5 passed** —— ① 一张凭证 / 5 行 / Σ借=Σ贷=1500；② 幂等 409 + 66112 且仍 1 张；③ 空草案 0 张；④ 已结账期间拒绝零残留；⑤ 1404 / 5401 是启用末级 |
| `raw/02-unit-tests.txt` | 纯逻辑单测 | **21 passed** |
| `raw/03-clippy.txt` | `cargo clippy -p cloudpivot --lib --tests -- -D warnings` | `rc=0`（零 warning） |
| `raw/04-fmt.txt` | `cargo fmt -p cloudpivot -p cloudpivot-kernel --check` | `rc=0` |
| `raw/05-cost-domain.txt` | `cargo test -p cloudpivot --lib business::erp::cost::` | **1093 passed / 0 failed**（M5-15 时 1067，+21 本卡纯单测、+5 真库用例，零回归） |
| `raw/06-residue.txt` | 真跑后残留检查 | `COST_VARIANCE` 凭证 0 行、`27xx-11` 期间 0 行，库已 drop |
| `raw/07-permission-audit.txt` | `system::menu::permission_audit_tests` | **8 passed**（无新增码，审计无变化） |

真库地址按卡面场景 A：**99 上跑，连 `172.18.10.145:5432`，用户 `cloudpivot`**。

> 注：卡面写的模板库 `cloudpivot_test` 在 145 上只有 **339 条迁移**，缺 `biz_voucher.currency_code`
> 等后加列，用它跑凭证写入会 500（`column "currency_code" of relation "biz_voucher" does not exist`）。
> 因此演练库克隆自同为演练用的 `cp_m511_test`（397 条迁移、科目表完整）。这条模板库漂移已在卡内报备，
> 与本卡代码无关（`biz_voucher` 的 DDL 属 M1-04，早于本卡）。

## 4. 端点契约（给调用方 / M5-36 收口）

```rust
// 纯逻辑：分摊结果 → 分录草案（可单测、零 IO）
let draft = cost_variance_voucher::build_draft(&period_outcome)?;          // 默认科目政策
let draft = cost_variance_voucher::build_draft_with(&policy, &outcome)?;    // 政策可覆盖

// 落库（不 commit，事务边界归调用方；期间没有非零差异 → Ok(None)，不造空凭证）
let posted = cost_variance_voucher::post_period_voucher(&tx, &ctx, &draft).await?;
//   ctx = VarianceVoucherContext { period_code, voucher_date, operator, now }
//   posted = Some(PostedVarianceVoucher { voucher_id, entry_count, total })

// 反查（红冲 / 报表用）：该会计期间未红冲的差异凭证
let ids = cost_variance_voucher::voucher_ids_of(&db, accounting_period_id).await?;
```

幂等键：`source_type='COST_VARIANCE'` + `source_id=会计期间 id`（`source_no` / `source_doc_no` = 期间码）。
判据与结算凭证同款：只算 `is_red = 0 AND reversed_by IS NULL`，红冲后可重出。
