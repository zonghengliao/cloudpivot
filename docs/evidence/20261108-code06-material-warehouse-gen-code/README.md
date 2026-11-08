# CODE-06 证据：物料 + 仓库表单「生成编码」按钮（交互方式 B）

卡：`t_141ed82e`（前端，后端预览接口同卡交付）
上游：CODE-01（`biz_seq` 六条纯流水号段）→ CODE-03（物料/仓库留空自动取号）→ CODE-05（供应商/客户同款按钮）→ 本卡

| 项 | 值 |
|---|---|
| 功能提交 | **`6e36984`** `feat(erp): CODE-06 物料/仓库「生成编码」按钮 + next-code 预览接口（交互方式 B）`（已 push origin/main） |
| 迁移 | **本卡没有迁移**（号段由 CODE-01 的 `V0337` 提供；预览复用 `seq::peek_master_code`） |
| 后端新增接口 | `GET /api/business/erp/material/next-code`、`GET /api/business/erp/warehouse/next-code` |
| 后端用例 | 新增 4 条真库用例全绿（物料 2 + 仓库 2）；`business::erp::handler::tests` 48 passed / 0 failed / 0 ignored；全量 `--lib` 3496 passed / 20 failed（20 条全是他卡既有红，§7.1 已 grep 归因） |
| 真实 token curl | **27 条断言全 PASS**（隔离实例 99 本机 :8046，全新库 `cloudpivot_code06a`） |
| 库侧断言 | **10 条全 PASS**（落库明细 / 形态正则 / 存活无重复 / 号段只被自动取号推进） |
| 前端门禁 | `pnpm typecheck` exit 0（`raw/05_frontend_typecheck.txt`）；`pnpm build` exit 0（`raw/06_frontend_build.txt`） |
| 前端单测 | 本卡相关 33 条全绿（新增 8 + 存量 25 修正后）；全量 1072 条里 3 条红 = 他卡既有（§7 归因） |
| 前端 UI 实测 | 浏览器真实点击「生成编码」+ 创建 + 编辑态禁用，**两页全部通过**（`raw/50..62`） |
| 未碰 | 144 生产、145 测试环境、计量单位口径、已应用迁移 |

---

## 1. 交互方式 B 怎么落地（两个表单一致）

```
┌──────────────────────────────────────────────┐
│ 物料编码  [ MAT-0004            ] [生成编码]  │
│           ⓘ 不点「生成编码」则保存时自动生成；  │
│             生成后仍可手改（已有编码体系可直接填）│
└──────────────────────────────────────────────┘
```

* 点按钮 → `GET /business/erp/{material,warehouse}/next-code` → 号填进编码框（可改）；
* 不点 → 保存时由后端取号（CODE-03 已有能力），保存成功后**把后端真正用的编码回填到表单**并在提示里报出；
* 编辑态：输入框与按钮**双双禁用**（编码是身份）。

## 2. 预览到底占不占号？—— **不占（方案 B，与 CODE-05 同口径）**

`seq::peek_master_code` 只 `SELECT biz_seq`，**不推进 `current_no`**：

| 方案 | 行为 | 本卡结论 |
|---|---|---|
| A 预览即占号 | 点一次按钮就烧一个号，取消建单浪费 | ❌ |
| **B 预览只算不占** | 连点两次同一个号；保存时才真取号；并发撞号由 `409 + 66001` 兜 | ✅ 采用 |
| C 预留 + 定时释放 | 要加释放任务，复杂度不成比例 | ❌ |

实测（`raw/10_mat_next1.json` vs `raw/11_mat_next2.json`）：两次预览都是 `MAT-0001`；
随后不填编码直接保存拿到 `MAT-0001`（`raw/12_mat_auto_absent.json`）；再预览变成 `MAT-0003`
（`MAT-0002` 被第二次自动取号用掉）—— 号是**保存**推进的，不是预览推进的。
仓库同构：`WH-01` / `WH-01` → 保存拿到 `WH-01` → 再预览 `WH-03`。

## 3. 接口契约（新增 2 个，权限码复用既有 `:create`）

| 方法 | 路径 | 权限码 | 出参 |
|---|---|---|---|
| GET | `/api/business/erp/material/next-code` | `business:erp:material:create` | `{"code":0,"msg":"ok","data":{"code":"MAT-0001"}}` |
| GET | `/api/business/erp/warehouse/next-code` | `business:erp:warehouse:create` | `{"code":0,"msg":"ok","data":{"code":"WH-01"}}` |

* **只读**：不写 `biz_seq`，不需要事务；
* **撞号顺延**：复用 `master_code_exists_sql`，历史占号自动跳过；
* **错误口径**：号段规则缺失 / 连号全被占 ⇒ `500 + 10004`（与真实取号同一套 `seq_error`）；
* **不新增权限码、不带迁移**：预览是「建单前看一眼」，用 `:create` 即可。

## 4. 代码改动（9 文件 +619 −37）

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/handler.rs` | 新增 `warehouse_next_code` / `material_next_code`（handler）+ `preview_warehouse_code` / `preview_material_code`（可单测业务主体）；模块头「特殊查询」4 → 6 条；`#[cfg(test)]` 新增 4 条真库用例 |
| `frontend/src/api/business/erp/index.ts` | 新增 `fetchMaterialNextCode()` / `fetchWarehouseNextCode()`（复用 CODE-05 的 `NextCodePreview`）；`CreateMaterialReq.material_code` / `CreateWarehouseReq.warehouse_code` → `string \| null` |
| `frontend/src/views/business/erp/material/index.vue` | 编码栏改「输入框 + 生成编码按钮」；`handleGenCode()`；保存后回填 `created.material_code` 并在提示里报出 |
| `frontend/src/views/business/erp/material/material-query.ts` | `validateMaterialForm` 去掉「编码必填」（保留超长）；`buildMaterialCreatePayload` 留空发 `null` |
| `frontend/src/views/business/erp/warehouse/components/WarehouseFormDialog.vue` | 同上（**仓库表单实际在这个弹窗里**，卡面写的 `warehouse/index.vue` 只是挂它） |
| `frontend/src/views/business/erp/warehouse/warehouse-tree.ts` | `validateWarehouseForm` 去掉「请填仓库编码」（保留超长）；`buildWarehouseCreatePayload` 留空发 `null` |
| `frontend/tests/unit/code06-gen-code-button.test.ts` | 新增 8 条单测（每个表单 4 条：校验口径 / 请求体 / API 路径 / 按钮+编辑态禁用+回填） |
| `frontend/tests/unit/erp-material-list.test.ts`、`warehouse-tree.test.ts` | 旧「编码必填」断言按新口径改成「留空不拦」 |

**关键连带修复（CODE-03 交接点名）**：前端原来拦「编码必填」，会让验收「不点生成直接保存」被前端自己挡住 —— 本卡删掉这两条校验，只保留长度校验（后端仍挡非法字符）。

## 5. 真实 token curl（隔离实例 :8046 + 全新库 `cloudpivot_code06a`）

脚本 `scripts/01_curl.sh`，原始响应在 `raw/*.json`，断言汇总 `raw/00_summary.txt`（**27 PASS / 0 FAIL**）：

| 段 | 断言 |
|---|---|
| ① 物料预览 | 形态 `^MAT-\d{4,}$` = `MAT-0001`；连点两次同号 |
| ② 物料保存 | 字段缺席 → 编码 == 预览号；`null` → `MAT-0002`；号被用掉后预览往后走 |
| ③ 物料手工码 | `YB-001-02` 原样保留 |
| ④ 物料撞号 | 预览号建单 200 且原样保留；同号再提交 → `409 + 66001` 且文案点名编码 |
| ⑤ 仓库预览 | 形态 `^WH-\d{2,}$` = `WH-01`；连点两次同号 |
| ⑥ 仓库保存 | 字段缺席 → `WH-01`；空串 → `WH-02` |
| ⑦ 仓库手工码/撞号 | `CK-001` 原样保留；预览号原样用；同号 `409 + 66001` 点名 |
| ⑧ 权限 | 不带 token 访问两个预览接口 → `401` |

## 6. 库侧断言（`scripts/02_db_assert.sh` → `raw/02_db_assert.txt`，10 PASS / 0 FAIL）

```
MAT-0001 | CODE06 物料（字段缺席） | RAW
MAT-0002 | CODE06 物料（null）     | RAW
YB-001-02| CODE06 物料（手工码）   | RAW
MAT-0003 | CODE06 物料（用预览号） | RAW
WH-01 | CODE06 仓库（字段缺席）
WH-02 | CODE06 仓库（空串）
CK-001| CODE06 仓库（手工码）
WH-03 | CODE06 仓库（用预览号）
⇒ MAT/WH 未删行无重复；biz_seq.current_no 均为 2（= 自动取号次数），
  手工提交预览号走「手工编码」分支不推进号段 —— 这就是「预览不占号」的库侧直接证据。
```

## 7. 测试

### 7.1 后端（真库 `cloudpivot_code06t`，`ERP_DB_URL`）

```
cargo test --lib -- business::erp::handler::tests --test-threads=1
→ test result: ok. 48 passed; 0 failed; 0 ignored; 0 measured; 3468 filtered out  (raw/03_backend_tests.txt)

cargo test --lib -- --test-threads=1（全量，1629s）
→ test result: FAILED. 3496 passed; 20 failed; 0 ignored; 0 measured; 0 filtered out  (raw/08_full_lib.txt)
   全部 20 条：asset_linkage_tests 11 + expense_linkage_tests 6 + flow::linkage 3
   —— 与 CODE-03 交接里记录的基线红**逐条同族**，且
   `grep -E '^test .*FAILED' | grep -iE 'material|warehouse|preview|next_code|erp::handler|erp::dto|erp::seq'` = **0 条**，
   即本卡改动的路径（handler 主数据 + 预览接口）**零红点**。
```
新增 4 条各自钉死什么（`raw/03_backend_tests.txt`）：

| 用例 | 钉死的行为 |
|---|---|
| `material_preview_code_does_not_consume_sequence` | 预览两次同号 → 保存拿到的正是预览号 → 保存后再预览往后走 |
| `material_previewed_code_is_used_verbatim_on_create` | 预览号当手工编码提交原样保留；已占用则预览跳过；再提交同号 `409+66001` 点名 |
| `warehouse_preview_code_does_not_consume_sequence` | 同构（WH 号段，2 位形态 `is_auto_wh_code`） |
| `warehouse_previewed_code_is_used_verbatim_on_create` | 同构 |

> 注：`ERP_DB_URL` 不设时 `setup()` 直接 return `None`，用例会**静默跳过**（假绿）。
> 本卡所有后端用例都在 `ERP_DB_URL` 指向的真库上跑，0 ignored / 0 skipped 可见于上面的 test result 行。

### 7.2 前端

```
npx tsx --test code06-gen-code-button erp-material-list warehouse-tree
→ # tests 33 / # pass 33 / # fail 0   (raw/03_frontend_unit_card06.txt)
pnpm test:unit 全量 → # tests 1072 / # pass 1069 / # fail 3   (raw/04_frontend_unit_all.txt)
pnpm typecheck → exit 0   (raw/05_frontend_typecheck.txt)
pnpm build     → exit 0   (raw/06_frontend_build.txt)
```

**3 条红全部是本卡之外的既有失败**（`raw/07_attribution.txt` 有 `git diff --stat HEAD` 为空的逐条证明）：
`erp-sales-order.test.ts`（sales-order 载荷多了 `currency_code`）、`flow-biz-types`（FlowBizType 字面量对齐）、
`p307-awaiting-executor.test.ts`（整文件失败）。三个来源文件与 HEAD **完全一致**，本卡改动清单不含其中任何一个。

### 7.3 前端 UI 真实点击（浏览器，`:5273` → 代理 `:8046`）

| 截图 | 内容 |
|---|---|
| `raw/50_ui_material_gen_code.png` | 物料新建弹窗：编码框 `MAT-0004` + 「生成编码」按钮 + help 文案 |
| `raw/51_ui_material_created.png` | 创建成功提示 **「物料已创建 · 编码 MAT-0004」**，列表出现该行 |
| `raw/60_ui_warehouse_gen_code.png` | 仓库新建弹窗：编码框 `WH-04` + 「生成编码」按钮 |
| `raw/61_ui_warehouse_created.png` | 创建成功提示 **「仓库「CODE06 仓库（UI 实测）」已创建 · 编码 WH-04」**，树出现该节点 |
| `raw/62_ui_warehouse_edit_disabled.png` | 编辑态：编码框 `disabled=true`、按钮 `t-is-disabled` + `disabled` 属性 |

## 8. 复现步骤

```bash
bash scripts/00_start_instance.sh start     # 库 cloudpivot_code06a / 127.0.0.1:8046
bash scripts/01_curl.sh                     # 27 断言 → raw/00_summary.txt
bash scripts/02_db_assert.sh                # 10 断言 → raw/02_db_assert.txt
bash scripts/00_start_instance.sh stop
```

后端用例（另建测试库，避免与 evidence 库的号段互相污染）：

```bash
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_code06t' \
  cargo test --lib -- business::erp::handler::tests --test-threads=1
```

## 9. 偏差与说明

1. **卡面写 `warehouse/index.vue`**，实际仓库表单在 `components/WarehouseFormDialog.vue`（index.vue 只挂弹窗）。改动落在真实文件上，单测额外钉一条「index.vue 仍引用该弹窗」防假绿。
2. **cargo test 全量**：本卡相关模块 48/48 绿；全量 `--lib` = 3496 passed / 20 failed / 0 ignored，红点集中在 `asset_linkage_tests` 11 + `expense_linkage_tests` 6 + `flow::linkage` 3（与 CODE-03 交接记录的基线逐条同族），`grep` 证明本卡改动路径（material/warehouse/preview/handler/dto/seq）**零红点**，属他卡既有红，不是本卡引入。
3. **实例二进制**来自共享工作区 `cargo build --bin cloudpivot`（HEAD + 本卡改动；工作区另有他卡未提交的 flow 子流程 WIP，编译通过；本卡接口断言只走 ERP 主数据路径）。
4. **环境收尾**：隔离实例已 stop（pid 文件在 scratch，:8046 已释放）；99 本机留下 `cloudpivot_code06a`（evidence 库）与 `cloudpivot_code06t`（后端真库用例）两个库，便于复跑，不碰 144 / 145。
