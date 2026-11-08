# CODE-07 前端：资产分类 + 费用类型表单「生成编码」按钮（交互方式 B）

> 卡片：`t_4cfddf93`（CODE-07），父卡 CODE-04 已提供后端预览接口，本卡补齐前端并对后端契约做真实 HTTP 复核。
> 分支/提交：见卡内汇报（`feat(erp): CODE-07 …` + 本证据目录）。

## 1. 一句话结论

资产分类（`asset-category/index.vue`）与费用类型（`expense-type/index.vue`）的编码栏都改成了
**「输入框 + 生成编码」按钮**（用户拍板的交互方式 B）：

- 点按钮 → `GET …/next-code` **先看号**（只读、不占号，点了又取消不浪费流水），号填进输入框，**仍可手改**；
- 不点按钮直接保存 → 前端发 `null`，**后端保存时自动取号**，保存成功把后端真正用的编码**回填**到表单并写进 toast；
- 编辑态编码框与「生成编码」按钮**双禁用**（编码是档案身份 / 报销科目快照口径，不支持改）；
- 前端不再拦「编码必填」，字符集统一到 `shared/master-code.ts`（字母 / 数字 / `-` `_` `.`，≤ 40，转大写后校验）——
  旧正则 `^[A-Z][A-Z0-9_]*$` 不接受连字符，会把取号器自己产出的 `AC-01` / `ET-01` 拦在门外。

验收用例 **两个表单各 4 条**（见 §4），全部 PASS；真 token curl **37 断言** + 库侧 **17 断言** 全绿；
浏览器真实点击 5 张截图（§6）；`pnpm typecheck` / `pnpm build` / `pnpm test:unit` / `cargo test` 结果见 §7。

## 2. 改动文件

| 文件 | 性质 | 说明 |
|---|---|---|
| `frontend/src/views/business/erp/shared/master-code.ts` | 新增 | 六个主数据表单共用的编码口径（`MASTER_CODE_PATTERN` / `MASTER_CODE_MAX_LEN` / `toMasterCodeInput` / `masterCodeError`），镜像后端 `seq::resolve_manual_code` + `validate_manual_code` |
| `frontend/src/views/business/erp/asset-category/index.vue` | 改 | 编码栏 → 输入框 + 「生成编码」按钮；编辑态双禁用；`handleGenCode` 调预览接口；保存回填编码 + toast 报编码 |
| `frontend/src/views/business/erp/expense-type/index.vue` | 改 | 同上；页面顶部提示文案从「编码大写字母开头（如 TRAVEL）」改成「编码留空保存时自动生成（ET-01 形态）」 |
| `frontend/src/views/business/erp/asset/asset-logic.ts` | 改 | `validateCategoryForm`：编码改「可留空 + 字符集」口径（不再拦必填） |
| `frontend/src/views/business/erp/expense/expense-logic.ts` | 改 | `validateTypeForm` / `toTypePayload` / `TYPE_CODE_PATTERN` 同口径；留空发 `null` |
| `frontend/src/api/business/erp/asset.ts` | 改 | 新增 `fetchAssetCategoryNextCode()`（路径 `${BASE}/asset-category/next-code`，`NextCodePreview` 复用 `index.ts`）；`category_code` 放宽为 `string \| null` |
| `frontend/src/api/business/erp/expense.ts` | 改 | 新增 `fetchExpenseTypeNextCode()`；`type_code` 放宽为 `string \| null` |
| `frontend/tests/unit/code07-gen-code-button.test.ts` | 新增 | 每个表单 4 条用例，共 8 条（校验口径 / 请求体 / API 路径 / 页面按钮与回填） |
| `frontend/tests/unit/fin2-asset.test.ts` | 改 | 旧「编码必填」断言改口径（编码可留空；`AC-01` / `CAT-BUILDING` 放行，空格仍拦） |
| `frontend/tests/unit/fin3-expense.test.ts` | 改 | 旧 `^[A-Z][A-Z0-9_]*$` 断言改口径（连字符 / 数字开头放行，空格仍拦） |
| `docs/evidence/20261108-code07-asset-category-expense-type-gen-code/**` | 新增 | 本证据目录（脚本 + 原始响应 + 截图 + 摘要） |

**后端零改动**：两个预览接口由父卡 CODE-04 交付（`asset_handler.rs::asset_category_next_code` /
`expense_handler.rs::type_next_code`，均 `seq::peek_master_code`，只读不占号），本卡只做真实 HTTP 复核（§5）。
因此本卡不新增迁移，也没改任何已应用的迁移。

## 3. 编码规则（用户已拍板，未改）

| 主数据 | 前缀 | 格式 | 实测产出 |
|---|---|---|---|
| 资产分类 | AC | 纯流水 2 位、不计日期、永不重置 | `AC-01` → `AC-02` → … |
| 费用类型 | ET | 纯流水 2 位、不计日期、永不重置 | `ET-01` → `ET-02` → … |

计量单位**不自动生成**（本卡未碰）；手工覆盖始终允许（`CAT-BUILDING-135529` / `ADMIN-FEE-135529` 实测原样落库）。
第 100 个起自然变 `AC-100` / `ET-100`（`seq::format_master_code` 只补零不截断，父卡单测 `t05` 覆盖）。

## 4. 验收用例矩阵（每表单 4 条）

证据：`raw/00_summary.txt`（curl 37 断言）、`raw/10_db_summary.txt`（库侧 17 断言）。

| # | 用例 | 资产分类（AC） | 费用类型（ET） | 结果 |
|---|---|---|---|---|
| ① | 点「生成编码」→ 预览形态 + 连点两次同号（只读不占号） | `AC-01` / `AC-01` | `ET-01` / `ET-01` | PASS |
| ② | 不点生成直接保存（字段缺席 / `null`）→ 自动取号，且 == 预览号 | `AC-01`、`AC-02` | `ET-01`、`ET-02` | PASS |
| ③ | 手工编码原样保留（小写提交 → 转大写落库） | `CAT-BUILDING-135529` | `ADMIN-FEE-135529` | PASS |
| ④ | 预览号当手工码提交 → 原样落库；再提交一次 → `409 + 66001` 且文案点名编码；号被用掉后预览往后走 | `AC-03` → `409 66001` → 预览 `AC-04` | `ET-03` → `409 66001` → 预览 `ET-04` | PASS |

补充断言（同一脚本）：

- ⑤ 无 token 访问两个预览接口 → **401**（认证层在，不是裸奔）；
- ⑥ 编辑（PUT）编码留空 = **不改编码**（`AC-01` / `ET-01` 保持），对应前端编辑态编码栏只读时的提交形态。

库侧补充：存量种子 5 条分类 + 8 条费用类型一行未动；`biz_seq` 的 `AC/ET current_no=2、pad_width=2`
（第 3 个号是手工路径提交，不推进计数器）；两表编码唯一索引仍在；全表无重复编码。

## 5. 真实 token curl（隔离实例）

```bash
# 全新空库 cloudpivot_code07e2e（首启自动应用 274 个迁移）+ 本卡二进制（HEAD 506fa34 隔离 worktree 构建）
bash scripts/00_start_instance.sh start     # 127.0.0.1:8047
bash scripts/01_curl.sh                     # 37 断言 → RESULT: ALL PASS（raw/00_summary.txt）
bash scripts/02_db_assert.sh                # 17 断言 → RESULT: ALL PASS（raw/10_db_summary.txt）
```

首登有 `must_change_password` 闸（业务接口 403），脚本自动改密 → 等 60s 会话缓存过期 → 重登（`raw/00_change_password.json`）。

> `raw/00_login.json` 里的 `access_token` / `refresh_token` 已替换成 `<redacted-jwt-in-evidence>`（有效期 15 分钟的隔离实例令牌，
> 但仍按 secret 口径处理）；令牌长度 461 的真实值记录在 `raw/00_summary.txt`。

## 6. 浏览器实测（真实点击，不是模拟）

环境：`BACKEND_ORIGIN=http://127.0.0.1:8047 npx vite --port 5178`（dev server 代理到本卡实例），Chrome 经 CDP 真点。

| 截图 | 实测内容 |
|---|---|
| `raw/ac_ui_01_gen_code_saved.png` | 新建资产分类：编码框空 → 点「生成编码」→ 填出 **`AC-04`**（说明预览不占号，前 3 个号已被 ②④ 用掉） |
| `raw/ac_ui_02_row_created.png` | 保存后列表出现 `AC-04 CODE07 UI 分类（点生成编码）`（预览号真的被用掉） |
| `raw/ac_ui_03_edit_disabled.png` | 编辑态：编码框灰显 `AC-04`、按钮灰显、help「编码是分类身份，不支持修改」（`code_disabled=true / btn_disabled=true`） |
| `raw/et_ui_01_gen_code_saved.png` | 新建费用类型：点「生成编码」→ `ET-04`，保存 toast = **`已新建费用类型 · 编码 ET-04`** |
| `raw/et_ui_02_edit_disabled.png` | 编辑态：编码框灰显 `ET-04`、按钮灰显（`code_disabled=true / btn_disabled=true`） |
| `raw/et_ui_03_auto_code_on_save.png` | **不点生成**直接保存（编码框为空）→ toast = **`已新建费用类型 · 编码 ET-05`**（后端取号 + 回填） |
| `raw/ac_ui_04_auto_code_on_save.png` | 同理资产分类 → toast = **`分类已创建 · 编码 AC-05`** |

> UI 实测里科目下拉统一选了第一项 `1001 库存现金`（脚本无脑点第一项，纯粹为了让表单过校验），
> 与编码逻辑无关；真要业务语义正确的科目请走正式数据。

## 7. 门禁

| 检查 | 命令 | 结果 |
|---|---|---|
| 前端类型 | `NODE_OPTIONS=--max-old-space-size=8192 pnpm typecheck` | **exit 0**（`raw/05_frontend_typecheck.log`） |
| 前端构建 | `NODE_OPTIONS=--max-old-space-size=6144 pnpm build` | **exit 0**（`raw/06_frontend_build.log`） |
| 本卡单测 | `npx tsx --test tests/unit/code07-gen-code-button.test.ts tests/unit/fin2-asset.test.ts tests/unit/fin3-expense.test.ts` | **71 passed / 0 failed**（其中本卡 8 条，`raw/03_frontend_unit_card07.log`） |
| 全量单测 | `pnpm test:unit` | 1077 passed / **3 failed**，3 条全部与本卡无关（归因见 §8，`raw/04_frontend_unit_all.log`） |
| 后端单测（真库） | `ERP_DB_URL=… cargo test --lib business::erp::{seq,asset_handler,expense_handler}` | **seq 11 / asset_handler 4 / expense_handler 4，0 failed**（`raw/03_backend_tests.log`） |
| 迁移幂等 | 全新空库首启 + 二次启动 | 首启「应用 274 个迁移」，二次启动 0 新增；不改任何已应用迁移 |

> `NODE_OPTIONS` 是为绕开本机 4 核 / 15G 上 `vue-tsc` 的默认 1.5G 堆上限（并发跑 cargo 时实测 OOM 一次，非代码问题）。

## 8. 全量单测那 3 条失败不是本卡（归因）

在**干净 HEAD 506fa34 的独立 worktree** 上跑同样三个文件（`raw/07_attribution.log`）：

| 失败用例 | 干净 HEAD 上 | 归因 |
|---|---|---|
| `erp-sales-order.test.ts`：create payload keeps dates…（多出 `currency_code: null`） | **同样失败** | 主线既有红点（SO-42 币种改造 f45e23e 未同步该断言），与本卡无关 |
| `flow-biz-link.test.ts`：FlowBizType 字面量 16 ≠ 15 | **同样失败** | 主线既有红点（P3-08 新增 flow 业务类型后前端清单未同步），与本卡无关 |
| `p307-awaiting-executor.test.ts`：`payable-logic` 缺导出 `executePaymentConfirmText` | **通过（16/16）** | 共享工作区里**别人未提交的 payable 前端改动**造成，与本卡无关 |

## 9. 环境坑（同批卡复用）

1. **`config/config-test.toml` 是 gitignore 的本机文件**：worktree 里没有它 ⇒ `[middleware.auth] enabled` 仍是基础配置的
   `false` ⇒ 认证层不挂载 ⇒ 所有 `#[nik::permission]` 接口回 `403 无权限访问`（登录能成功、token 带 `*:*:*` 也没用）。
   解决：把 `/opt/cloudpivot/backend/config/config-{test,dev}.toml` 拷到 worktree 的 `backend/config/`。
2. **全新空库首启时迁移早于 bootstrap**：`sys_role` 为空 ⇒ `V0151` 等授权迁移「跳过」，admin 无角色绑定
   （`sys_user_role` 0 行，接口 403）。**再启动一次**即补齐（实测 `sys_user_role` → 1，admin = `SUPER_ADMIN`）。
3. 本机 `vue-tsc` 默认堆上限会被 OOM 杀掉（exit 134），需要 `NODE_OPTIONS=--max-old-space-size=8192`。

## 10. 明确没做的（按卡面约束）

- ❌ 144 生产（卡面禁止）；145 常驻实例未重启（无人值守会话禁止 `systemctl restart`）。
- ❌ 不改计量单位（卡面禁止）。
- ❌ 手工指定编码开关（CODE-08）。
- 无新增迁移、无后端改动（预览接口由父卡 CODE-04 交付，本卡只复核）。
