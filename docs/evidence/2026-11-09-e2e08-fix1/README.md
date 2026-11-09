# E2E08-FIX-1 证据：阶梯价区间不相交校验 + 客户编码锁定

- 卡片：`t_fcb017a7`（E2E-08 演练 2 条 P3）
- 代码提交：`33961c15`（后端 3 文件 + 演练脚本 2 文件）；脚本修正 `541d246e`
- 分支：`origin/main`（`git ls-remote origin main` 复核见 §6）
- 验证树：`/opt/cloudpivot`（99 工作仓）；145 演练机 `/opt/cloudpivot` 已 ff 到同一提交

---

## 0. 一句话结论

| 卡面问题 | 修法 | 实测 |
|---|---|---|
| ① 同物料/客户/类型/生效期内，`5~20` 与 `1~10` 可同时 `ACTIVE`（配置层自相矛盾） | 建 **和** 改档位时按「四维同键 + 生效期相交 + 数量区间相交」判重 → **409 + 10003**，文案点名「已有 5~20，本次 1~10」 | 145 真 token：**409**，文案含 `5~20`/`1~10`/`2031-01-01~2031-12-31`；21~30、不同物料、生效期错开、PROMO 与 TIER 并存全部 **200** |
| ② 客户编码可被 `PUT` 改写（六类口径不一致，只有客户能改） | `update_customer` 传了非空且与现值不同的 `code` → **400 + 10001**「客户编码不可修改」（与资产分类同口径） | 145 真 token：**400**，库里编码未变，同一请求里的 `contact_phone` 也没落库（整体 400） |

**没有回滚任何产品口径**：卡面注「需产品拍板」的部分按卡面要求实现为「必须不重叠」；
取价侧「多条命中取 `min_qty` 最大」的兜底规则**没有改动**（历史数据 / `PROMO` 盖 `TIER` 时仍按它给确定答案，
见 `price_model.rs` 文件头 §确定性 与该模块既有用例 `tier_overlap_picks_highest_min_qty`，本轮 92 条 price 用例全绿）。

---

## 1. ①的判据（与取价侧同口径，不做第二套）

| 维度 | 相交判据 | 边界 |
|---|---|---|
| 物料 | `material_id` 相等 | — |
| 客户 | `customer_id` 相等，**`NULL` 与 `NULL` 算同一个「通用档」** | SQL 用 `IS NOT DISTINCT FROM(NULLIF(?::text,'')::uuid)`（与 `uk_biz_price_tier ... NULLS NOT DISTINCT` 同口径） |
| 档位类型 | `tier_type` 相等 | `TIER` 与 `PROMO` **合法并存**（促销盖在阶梯上是业务常态） |
| 数量区间 | 半开 `[min_qty, max_qty)`：`NOT (a.max <= b.min OR b.max <= a.min)`，`max = NULL` = 无上限 | `1~10` 与 `10~50` **不算重叠**（qty=10 只归第二档） |
| 生效期 | 闭区间 `[from, to]`：`NOT (a.to < b.from OR b.to < a.from)`，`to = NULL` = 长期有效 | 端点相接（`…~12-31` 与 `12-31~…`）**算重叠**（那天两档都有效） |

- 判据写成**纯函数** `qty_ranges_overlap` / `periods_overlap`（`price_admin_model.rs`），SQL 只按
  「物料 + 客户 + 类型 + `status='ACTIVE'` + 未删」粗筛窄列 —— 边界因此能被 `cargo test --lib` 钉住，
  不依赖真库、也不会出现「SQL 一份、Rust 一份」先腐化。
- **只对两边都 `ACTIVE` 的行校验**：停用档不参与取价，允许留重叠草稿；把停用档改回 `ACTIVE`
  走的是同一个函数（`update_tier`），那时会被拦（用例 `inactive_tier_may_overlap_but_reenable_is_blocked`）。
- 建（`create_tier`）与改（`update_tier`）**都拦**；改时 `exclude = Some(id)`，否则「只改价格、区间不动」
  会被自己判成重叠。

## 2. ②的判据

`customer_handler::update_customer` 在合并写入值**之前**：

```text
传了 code → normalize（去空白）
   ├─ 空串 / 纯空白 → 交给既有入参校验（400「code 不能为空」，编码是必填列，不存在「清空」）
   ├─ 等于现值     → 放行（前端整体提交表单 / 只改其他字段都走这条）
   └─ 不等现值     → 400 + 10001「客户编码不可修改（当前 X / 请求 Y）：编码是历史单据与审计的引用口径…」
```

`customer_model::customer_update` 仍**整行写回**（`CustomerWrite` 保持「整行真相」语义），
但此时 `write.code` 恒等于当前行编码 —— 已在函数文档里写死这条不变式。
前端 `buildCustomerUpdatePayload` 是**差量提交**、从来不发 `code`（`customer-query.ts:471`），
所以正常界面操作不会被这条锁挡到；本次未改前端。

## 3. 六类（实为七项）编码字段接受度实测

145 真 token curl，`⑤` 段逐项贴 HTTP 码 + 库内编码前后对比：

| 主数据 | PUT 带新编码 | HTTP | 库内编码 | 机制 |
|---|---|---|---|---|
| **客户** | `{"code":"E2E08F1-C1-CHG"}` | **400 / 10001** | 未变 | 本次新增：handler 前置判定 |
| 资产分类 | `{"category_code":"…","category_name":…,"useful_life_months":…}` | **400 / 10001** | 未变 | 既有基准（`asset_handler.rs:261-271`） |
| 供应商 | `{"supplier_code":"E2E08F1-SUP-CHG"}` | 200 | 未变 | `UpdateSupplierReq` **没有该字段** → serde 忽略 |
| 物料 | `{"material_code":"E2E08F1-MAT-CHG"}` | 200 | 未变 | `UpdateMaterialReq` 没有该字段 |
| 仓库 | `{"warehouse_code":"E2E08F1-WH-CHG"}` | 200 | 未变 | `UpdateWarehouseReq` 没有该字段 |
| 单位 | `{"unit_code":"E2E08F1-U-CHG"}` | 200 | 未变 | `UpdateUnitReq` 没有该字段 |
| 费用类型 | `{"type_code":"E2E08F1-ET-CHG", …其余字段回填…}` | 200 | 未变（名称也没被带跑） | `TypeReq` 有这个字段，但 `type_update` 的 UPDATE 不写 `type_code` |

静态核查原文见 `six_types_static.txt`（四个 `Update*Req` 里 `*_code` 出现次数 = 0；
`expense_model::type_update` 的 SET 子句无 `type_code`）。

**结论**：七项**都改不动**（验收 ⑤ 的口径）。差异只在「表达方式」：客户 / 资产分类是显式 400、
其余是「字段压根不在契约里 / 不在写语句里」。若要统一成显式 400（对旧字段报警而不是静默忽略），
属于跨六类的契约收敛，建议另开卡（会动 4 个 DTO + 前端表单字段），本卡不越界。

## 4. 单测（真库 + 纯函数）

```
# 真库（隔离库 cloudpivot_e2e08fix1 = 从 cloudpivot_test_pdf07v2 克隆，295 迁移 / max V0362）
→ unit_price_tier_overlap.txt   ：22 passed / 0 failed（price_admin_handler::tests = 14 条既有 + 3 条纯函数边界 + 5 条真库）
→ unit_customer_code_lock.txt   ：14 passed / 0 failed（customer_handler::tests，含新增 customer_code_cannot_be_changed）
→ cargo test --lib business::erp::price -- --test-threads=1 ：92 passed / 0 failed（取价侧未被影响）
# 无库全量（真库用例按既有约定静默 SKIP，只有编译器+纯逻辑）
→ full_lib_no_db.txt            ：3971 passed / 0 failed / 0 ignored（5.68s）
```

真库用例覆盖卡面 ③ 四条：建重叠 → 409（且**不落库**）、建不重叠 → 200、不同物料同区间 → 200、
改客户编码 → 400、改客户其他字段 → 200；另加 4 条边界：改档位造成重叠 → 409 且不落库、
停用档可留重叠草稿但重新启用被拦、生效期端点相接算重叠、「通用档之间」`NULL` 视为同客户。

## 5. 145 真机实测（真 token curl）

```bash
# 145：git ff 到 33961c15 → cargo build --bin cloudpivot（2m05s）
# 起第二个实例（不动 8033 的 dev 服务、也不动别的卡占着的 8034）
NIK_SERVER_LISTEN=127.0.0.1:8036 ./target/debug/cloudpivot &
curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8036/health   # 200
BASE=http://127.0.0.1:8036/api bash scripts/verify/e2e08fix1_price_code_evidence.sh
```

- 结果：**PASS=46 FAIL=0**（原始输出 `curl_145_pass.txt`，含每条 409/400 的文案原文与库内前后值）
- 实例 8036 跑完即 `kill`（优雅关停日志「插件已全部停止」），未重启 8033（仍跑旧二进制）；
  本卡交付的是代码 + 脚本，上线由发布流程换二进制。
- 二进制来源 = `33961c15`；`541d246e` 只改取证脚本（无 Rust 改动）。
- 145 共享 dev 库（`cloudpivot_dev`）已确认在 V0362（`298 迁移 / max 362`）→ 起实例时「应用 0 个迁移」，未改库结构。
- 夹具全部落在 `E2E08F1-*` 前缀并当场清掉（脚本末尾断言「残留档位 = 0」）；
  ⑤ 段为了证明「字段被忽略」对七类各 PUT 了一次（供应商/物料/仓库/单位只发编码字段 → merge 保留其余；
  费用类型回填了全部字段），副作用仅 `updated_at` / `update_by` 变化。

## 6. 门禁与已知既有红

- `cargo fmt`：本卡 4 个 .rs 文件在**默认 rustfmt 与项目口径 rustfmt 1.94.1** 下都 `--check` 通过
  （`clippy_and_fmt.txt`）。全仓仍有 1 个**别人**的文件差异（`shared/state_machine.rs`），未碰。
- `cargo clippy -- -D warnings`：**4 条既有红，全部在本卡未修改的文件**（`git status --porcelain` 均为空）：
  `attachment/model.rs:879`（too_many_arguments）、`erp/invoice.rs:1912`（needless_lifetimes）、
  `erp/receivable.rs:782`（doc_lazy_continuation）、`erp/sales_order_model.rs:661`（too_many_arguments）。
  本卡改过的 3 个文件 0 告警。原文见 `clippy_and_fmt.txt`。
- 迁移：本卡**没有**新增/修改迁移（纯 handler + 校验，无 DDL 需求）。
- 前端：`pnpm typecheck` 与 `pnpm build` 均通过（未改前端；`buildCustomerUpdatePayload` 本来就差量提交、不发 `code`）。

## 7. 文件清单

```
backend/src/business/erp/price_admin_model.rs    +TierWindow / tier_active_windows / qty_ranges_overlap / periods_overlap
backend/src/business/erp/price_admin_handler.rs   +check_tier_overlap / tier_overlap / qty_text / period_text + create/update 接线 + 8 条用例（3 纯函数 + 5 真库）
backend/src/business/erp/customer_handler.rs      +编码锁（400）+ 2 条用例（1 条改既有期望 409→400）
backend/src/business/erp/customer_model.rs        +「编码整行写回」不变式文档
scripts/verify/e2e08_master_c.sh                  ⑥-3 由「口径探测」改为「必须 409」，加 ⑥-0b 客户编码锁断言
scripts/verify/e2e08fix1_price_code_evidence.sh   本次 145 实测脚本（46 条断言，可重跑幂等）
docs/evidence/2026-11-09-e2e08-fix1/*             本目录证据
```
