# CODE-05 证据：供应商 + 客户表单「生成编码」按钮（交互方式 B）

卡：`t_1c51dc99`（前端，后端预览接口同卡交付）
上游：CODE-01（`biz_seq` 六条纯流水号段）→ CODE-02（供应商/客户留空自动取号）→ 本卡（前端按钮 + 预览接口）

| 项 | 值 |
|---|---|
| 功能提交 | **`bcf4a77`** `feat(erp): CODE-05 供应商/客户「生成编码」按钮 + next-code 预览接口（交互方式 B）`（rebase 到 `2b816b3` 之上，已 push origin/main） |
| 迁移 | **本卡没有迁移**（号段由 CODE-01 的 `V0337` 提供；预览复用 `seq::peek_master_code`） |
| 后端新增接口 | `GET /api/business/erp/supplier/next-code`、`GET /api/business/erp/customer/next-code` |
| 后端用例 | 新增 4 条真库用例全绿（`preview_*` 2 + `*_previewed_code_*` 2）；相关 4 组套件 23 + 13 + 11 + 7 全绿 |
| 真实 token curl | **30 条断言全 PASS**（隔离实例 99 本机 :8045，全新库 `cloudpivot_code05a`） |
| 库侧断言 | **11 条全 PASS**（落库明细 / 形态正则 / 存活无重复 / 号段推进） |
| 前端门禁 | `pnpm typecheck` exit 0（`raw/05_frontend_typecheck.log`）；`pnpm build` exit 0（`raw/06_frontend_build.log`）；新增 7 条前端单测全绿 |
| 前端 UI 实测 | 浏览器真实点击「生成编码」+ 保存 + 编辑态禁用，**两页全部通过**（见 §6） |
| 145 测试环境 | `git pull` 到 `bcf4a77` + `cargo build` 0 错 + `systemctl restart cloudpivot-test`（health 200）；真 token 抽查两个 next-code 接口均 `200`（`SUP-0004` / `CUS-0006`） |

---

## 1. 交互方式 B 到底怎么落地

用户拍板：**编码栏 + 「生成编码」按钮**。点按钮 → 调预览接口 → 填进编码框（可改）；
不点 → 保存时后端取号，保存成功后回填显示。

```
┌──────────────────────────────────────────────┐
│ 供应商编码  [ SUP-0002            ] [生成编码] │
│             ⓘ 不点「生成编码」则保存时自动生成； │
│               生成后仍可手改（已有编码体系可直接填）│
└──────────────────────────────────────────────┘
```

### 1.1 预览到底占不占号？—— **不占（方案 B）**

代码里 `seq::peek_master_code` 只 `SELECT biz_seq`，**不推进 `current_no`**：

| 方案 | 行为 | 本卡结论 |
|---|---|---|
| A 预览即占号 | 点一次按钮就烧一个号，取消建单浪费 | ❌ |
| **B 预览只算不占** | 连点两次同一个号；保存时才真取号；并发撞号由 `409 + 66001` 兜 | ✅ **采用** |
| C 预留 + 定时释放 | 要加释放任务，复杂度不成比例 | ❌ |

实测证据（`raw/10_sup_next1.json` vs `raw/11_sup_next2.json`）：两次预览都是 `SUP-0002`；
随后不填编码直接保存，拿到的正是 `SUP-0002`（`raw/12_sup_auto_absent.json`）；
再预览变成 `SUP-0003` —— 号是被**保存**推进的，不是被预览推进的。

### 1.2 撞号兜底

两个人同时点「生成编码」看到同一个号，先保存的人赢：
后保存的人手工提交的号撞唯一索引 ⇒ `409 + 66001`，文案点名编码
（实测 `raw/20_sup_stolen.json`：`供应商编码 \`SUP-0005\` 已存在（已软删的编码可以复用）`）。
前端把后端原文弹在弹窗顶部（`submitError`），用户点一次「生成编码」换个号即可。

---

## 2. 接口契约（新增 2 个，权限码复用既有 `:create`）

| 方法 | 路径 | 权限码 | 出参 |
|---|---|---|---|
| GET | `/api/business/erp/supplier/next-code` | `business:erp:supplier:create` | `{"code":0,"msg":"ok","data":{"code":"SUP-0002"}}` |
| GET | `/api/business/erp/customer/next-code` | `business:erp:customer:create` | `{"code":0,"msg":"ok","data":{"code":"CUS-0004"}}` |

* **只读**：不写 `biz_seq`，不需要事务；
* **撞号顺延**：复用 `master_code_exists_sql`，历史占号（`SUP-0001` 种子、`CUS-0001..0003` 种子）自动跳过；
* **错误口径**：号段规则缺失 / 连号全被占 ⇒ `500 + 10004`（与真实取号同一套 `seq_error`）；
* **不新增权限码**：预览是「建单前看一眼」，用 `:create` 即可（不写迁移，避免菜单膨胀）。

路由注册顺序放在 `/{id}` 之前（`supplier_next_code` → `supplier_detail`），
静态段优先级 + axum 匹配都不会把 `next-code` 当成 id（实测 `GET .../supplier/next-code` 返回 200 而不是 uuid 解析失败）。

---

## 3. 代码改动

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/handler.rs` | 新增 `supplier_next_code`（handler）+ `preview_supplier_code`（可单测业务主体）；`#[cfg(test)]` 新增 2 条真库用例 |
| `backend/src/business/erp/customer_handler.rs` | 新增 `customer_next_code` + `preview_customer_code`；`#[cfg(test)]` 新增 2 条真库用例；模块头接口表补一行 |
| `frontend/src/api/business/erp/index.ts` | 新增 `NextCodePreview` 类型 + `fetchSupplierNextCode()` / `fetchCustomerNextCode()`；`CreateSupplierReq.supplier_code` / `CreateCustomerReq.code` 改成 `string \| null`（留空 = 自动取号） |
| `frontend/src/views/business/erp/supplier/supplier-query.ts` | `validateSupplierForm` 去掉「编码必填」（**保留超长校验**）；`buildSupplierCreatePayload` 留空发 `null` |
| `frontend/src/views/business/erp/customer/customer-query.ts` | 同上（客户侧） |
| `frontend/src/views/business/erp/supplier/index.vue` | 编码栏改成「输入框 + 生成编码按钮」；`handleGenCode()`；保存后回填 `created.supplier_code` 并在提示里报出编码 |
| `frontend/src/views/business/erp/customer/index.vue` | 同上（客户侧） |
| `frontend/tests/unit/code05-gen-code-button.test.ts` | 新增 7 条单测（校验口径 / 请求体 / API 路径 / 两页按钮与编辑态禁用；源码即契约） |
| `frontend/tests/unit/erp-customer.test.ts` | 旧断言 `errors.code === '客户编码必填'` 改成「留空不拦」（CODE-05 口径变化） |

**关键连带修复（CODE-02 交接点名）**：前端 `validateSupplierForm` / `validateCustomerForm`
原来拦「编码必填」，会让验收 ④（不点生成直接保存）被前端自己挡住 —— 本卡删掉了这条校验，
只保留长度校验（后端仍挡非法字符）。

---

## 4. 后端测试（隔离 worktree，真库）

> 为什么在 worktree 跑：共享工作区当时有**别卡未提交的子流程改动**（`flow/graph.rs` 加了
> `NodeType::Subprocess`，消费者还没改），整个 crate 编译不过。本卡在
> `/root/cp-code05-wt`（HEAD `17389af` + 本卡 2 个文件）验证，与别人的半成品零耦合。

```
=== 1) preview_  → 7 passed（含本卡 2 条）   EXIT 0
=== 2) supplier_ → 23 passed（含本卡 2 条）  EXIT 0
=== 3) customer_handler::tests → 13 passed  EXIT 0
=== 4) seq::tests → 11 passed               EXIT 0
```

新增 4 条用例各自钉死什么：

| 用例 | 钉死的行为 |
|---|---|
| `supplier_preview_code_does_not_consume_sequence` | 预览两次同号 → 保存拿到的正是预览号 → 保存后再预览往后走 |
| `supplier_previewed_code_is_used_verbatim_on_create` | 预览号当手工编码提交原样保留；已占用则预览跳过；再提交同号 409+66001 且文案点名 |
| `preview_code_does_not_consume_sequence`（客户） | 同上（CUS 号段） |
| `previewed_code_is_used_verbatim_on_create`（客户） | 同上 |

原始日志：`raw/03_backend_tests.txt`。

---

## 5. 真实 token curl（隔离实例 :8045 + 全新库）

实例：`scripts/00_start_instance.sh`（`profile=test`、库 `cloudpivot_code05a`、端口 8045、
二进制来自上面的 worktree，**与共享 target 的其他卡构建隔离**）。
断言脚本：`scripts/01_curl.sh`，原始响应在 `raw/*.json`，汇总在 `raw/00_summary.txt`。

**30 条断言全 PASS**，覆盖卡面验收 ①–⑥：

| 验收 | 实测 |
|---|---|
| ① 预览接口 | 两次 `GET .../supplier/next-code` 都是 `SUP-0002`；`.../customer/next-code` 都是 `CUS-0004` |
| ② 点按钮 → 填 SUP-000X | 形态正则 `^SUP-[0-9]{4}$` / `^CUS-[0-9]{4}$` 通过 |
| ③ 改掉成 GYS-001 → 保存成功 | `POST {"supplier_code":"GYS-001"}` → 200，返回 `GYS-001`（客户侧 `KH-001` 同） |
| ④ 不点按钮直接保存 | `POST` 不带编码 → 200，返回的正是刚才预览的号，且号段随之推进 |
| ⑤ 编辑态编码只读 | `PUT` 只带名称 → 200，编码不变（`SUP-0002` / `CUS-0004`） |
| ⑥ 客户同样四条 | 预览 / 自动 / 手工 / 重复 409+66001 全过 |
| 额外 | 撞号兜底 409+66001；非法编码 `BAD CODE` → 400+10001 |

库侧断言（`scripts/02_db_assert.sh` → `raw/02_db_assert.txt`，**11 条全 PASS**）：

```
 supplier_code |         supplier_name
 GYS-001       | CODE05 手工编码供应商
 SUP-0001      | 示例供应商（种子）
 SUP-0002      | CODE05 自动编码供应商（改名）
 SUP-0003      | CODE05 自动编码供应商（空串）
 SUP-0004      | CODE05 自动编码供应商（null）
 SUP-0005      | CODE05 用预览号建单供应商
   code   |            name
 CUS-0001 | 北京云枢科技有限公司        （种子）
 CUS-0004 | CODE05 自动编码客户（改名）
 CUS-0005 | CODE05 自动编码客户（null）
 KH-001   | CODE05 手工编码客户
```

* 自动号形态全部合法（`^SUP-[0-9]{4}$` / `^CUS-[0-9]{4}$`）；
* 手工码原样落库（`GYS-001` / `KH-001`）；非法编码没落库；
* 存活行内编码无重复；
* 号段只被**保存**推进：`SUP current_no=4 / 已用最大流水=5`（`SUP-0005` 是手工提交的，不走号段），
  `CUS current_no=5 / 已用最大流水=5` —— 期间一共点了 4 次供应商预览、3 次客户预览，**没有一号是被预览吃掉的**。

### 5.1 实例启动踩的两个坑（已写进脚本注释）

1. **app 按 CWD 读 `config/` 和 `migrations/`** —— 二进制放在 `/root/cp-code05-bin` 但必须
   `cd` 到 `<repo>/backend` 再启动（否则「数据库未初始化」直接 panic）；
2. **`config/config.toml` 里 `[middleware.auth] enabled = false`**，`profile=test` 要靠
   `config/config-test.toml` 覆盖（仓库只给 `.example`）。少了它 → 认证层不注入 `AuthClaims`
   → 所有 `#[permission]` 路由 403，且业务接口 500「Missing request extension AuthClaims」。

---

## 6. 浏览器实测（真实点击，不是模拟）

**环境**：`BACKEND_ORIGIN=http://127.0.0.1:8045 npx vite --port 5175`（dev server 走本卡二进制 + 全新库），
Chrome 真点按钮（CDP），截图见 `raw/50_ui_cus_edit_disabled.png`、`raw/51_ui_sup_new_gen.png`。

| 卡面验收 | 实测结果 |
|---|---|
| ① 新建供应商 → 编码栏空白 + 旁边有「生成编码」 | 编码框 `value=''`，placeholder「留空自动生成，如 SUP-0002」，右侧「生成编码」按钮 **enabled**，help「不点「生成编码」则保存时自动生成；生成后仍可手改（已有编码体系可直接填）」 |
| ② 点按钮 → 填入 SUP-000X | 点一次 → 编码框变 `SUP-0006`（列表当时最大 SUP-0005） |
| ③ 改掉成 GYS-001 → 保存成功且编码是 GYS-001 | 改成 `GYS-UI-001` → 提示「供应商已创建 · 编码 GYS-UI-001」，列表 6→7 条并出现该行 |
| ④ 不点按钮直接保存 → 自动生成 + 回填 | 编码框留空 → 提示「供应商已创建 · 编码 **SUP-0006**」（正是 ② 预览过的号，说明③没消耗号）+ 列表 7→8 条 |
| ⑤ 编辑已有供应商 → 编码只读、按钮禁用 | 编辑 `GYS-UI-001`：编码框 `disabled=true`（灰底）、「生成编码」`disabled=true`、help「编码是供应商身份，不支持修改」 |
| ⑥ 客户同样四条 | 新建 → 空 + 按钮 enabled；点生成 → `CUS-0006`；改名保存 → 提示「客户已创建 · 编码 CUS-0006」；不点生成保存 → `CUS-0007`；点生成后改成 `KH-UI-001` → 提示「客户已创建 · 编码 KH-UI-001」；编辑 `KH-UI-001` → 编码框与按钮都 `disabled=true` |

最漂亮的一条证据：**SUP-0006 被预览了两次都没被吃掉** —— 用户改成了 `GYS-UI-001`，
于是下一个「不点生成直接保存」的供应商正好拿到 `SUP-0006`；库侧 `current_no=6` 且
`已用最大流水=6`（`raw/02b_db_assert_after_ui.txt`），没有任何号为预览白跳。

---

## 7. 前端门禁
```
pnpm typecheck   (NODE_OPTIONS=--max-old-space-size=6144)  → exit 0
pnpm build       (vue-tsc --noEmit && vite build)          → exit 0
npx tsx --test tests/unit/code05-gen-code-button.test.ts   → 7 passed
npx tsx --test tests/unit/erp-customer.test.ts tests/unit/erp-master-data.test.ts → 38 passed
```

> 默认堆（无 `NODE_OPTIONS`）跑 `vue-tsc` 会 OOM —— 环境/包体问题，不是本卡引入的。

---

## 8. 与卡面的差异（如实记录）

1. **预览接口不新增权限码**：卡面只给了接口路径，没说权限口径。本卡复用 `:create`
   （「建单前看号」与建单同权），因此**不需要迁移**、不动菜单。
2. **卡片验收 ①「编码栏空白」**：新建时编码栏是空串（`emptySupplierForm().supplier_code = ''`），
   placeholder 从「如 SUP-0002（未删除内唯一）」改成「留空自动生成，如 SUP-0002」，
   旁边就是「生成编码」按钮 —— 与卡面 ASCII 图一致。
3. **保存后回填**：弹窗创建成功即关闭，所以「回填」体现为两处 —— (a) 表单字段被赋成后端真正用的编码；
   (b) 成功提示直接写 `供应商已创建 · 编码 SUP-0002`。列表刷新后该行编码也在。
4. **客户/供应商请求体留空发 `null`**（不是 `''`）：后端两种都当「没填」，但 `null` 语义更准；
   对应地把 `CreateCustomerReq.code` / `CreateSupplierReq.supplier_code` 类型放宽成 `string | null`。
