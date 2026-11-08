# CODE-08 证据：系统参数「主数据编码允许手工指定」开关（默认开）

任务卡：`t_85447e33`（CODE-08）。目标：企业已有编码体系时能对接，也能一键强制统一。

## 一句话结论

一条系统参数 `erp.master_data.allow_manual_code`（`BOOLEAN`，默认 `true`）统管 6 个主数据
（供应商 / 客户 / 物料 / 仓库 / 资产分类 / 费用类型）：`true` 可手填企业自有编码（现状），
`false` 时后端建单入口对**非空编码**一律 `400 + 66054`「系统已禁止手工指定编码」，前端编码栏
只读、「生成编码」按钮隐藏、提示改成「编码由系统自动生成」。**留空永远合法**（自动取号不受开关影响）。

实测（99 本机隔离实例，全新空库 `cloudpivot_code08c`，:8048）：真实 token curl **60 条断言全 PASS**，
库侧 **38 条断言全 PASS**，后端 **10 条新单测 + 116 条相关模块用例全绿**（`ERP_DB_URL` 真库、
无 ignore/skip），迁移首启 275 个 / 二次启动 0 个（幂等）。

## 交付物

| 层 | 文件 | 说明 |
|---|---|---|
| 迁移 | `backend/migrations/business/V0342__erp_master_data_code_policy.{up,down}.sql` | 种参数行 + `erp_master_data` 分类字典项 + 自检；`ON CONFLICT … DO NOTHING` 幂等；down 只删自己种的行 |
| 后端 | `backend/src/business/erp/code_policy.rs`（新增） | 键常量 / 读取（快照）/ 纯函数解析 / 建单闸 / 策略接口 / 10 条用例 |
| 后端 | `backend/src/business/erp/handler.rs` | 供应商、物料、仓库建单接闸；供应商拆出 `create_supplier_with_policy`，另加 `GET /business/erp/master-data/code-policy` |
| 后端 | `backend/src/business/erp/customer_handler.rs` | 客户建单接闸 |
| 后端 | `backend/src/business/erp/asset_handler.rs` | 资产分类建单接闸 |
| 后端 | `backend/src/business/erp/expense_handler.rs` | 费用类型建单接闸 |
| 后端 | `backend/src/business/erp/mod.rs` | 注册 `code_policy` 模块 |
| 后端 | `backend/src/shared/error_code.rs` | 新码 `ERP_MANUAL_CODE_FORBIDDEN = 66054`（+ 66xxx 段稳定性自检） |
| 前端 | `frontend/src/api/business/erp/index.ts` | `MasterDataCodePolicy` 类型 + `fetchMasterDataCodePolicy()`（`${BASE}/master-data/code-policy`） |
| 前端 | `frontend/src/views/business/erp/shared/code-policy.ts`（新增） | 策略加载器（Promise 缓存 + fail-open） |
| 前端 | `.../shared/code-policy-pure.ts`（新增） | 无依赖纯逻辑（兜底值 / 提示文案 / 禁用判定），可单测 |
| 前端 | 6 个表单 | 编码栏 `:disabled="manualCodeDisabled(isEdit, allowManualCode)"`、按钮 `v-if="allowManualCode"`、提示走 `manualCodeHelp` |
| 单测 | `frontend/tests/unit/code08-manual-code-switch.test.ts`（新增） | 5 条纯逻辑/契约 + 6 条表单源码钉死 |

## 参数口径

```
key:      erp.master_data.allow_manual_code
type:     BOOLEAN            （`support::config::model::config_type` 唯一口径，前端据此渲染开关）
默认:     true               （= 改动前行为，升级零变化）
分类:     erp_master_data     （字典 config_category = 「ERP 主数据」）
可修改:   true（管理端可切换）；未加密；不需重启（写后自动重载运行时快照）
选项:     true|开启,false|关闭
```

## 后端行为

| 场景 | 结果 |
|---|---|
| 开关 `true` + 手工编码 | 200，编码原样保留（`GYS-001` / `YB-001-02` / `CAT-BUILDING` …） |
| 开关 `true` + 留空 | 200，自动取号（`SUP-0001` / `WH-01` …，CODE-01~07 既有能力） |
| 开关 `false` + 手工编码 | **400 + 66054**，文案「系统已禁止手工指定编码：…（系统参数 erp.master_data.allow_manual_code = false）…」 |
| 开关 `false` + 留空 | 200，照常自动取号（开关只管「手填」，不改取号） |
| 参数缺失 / 停用 / 值非法 | 回退 `true`（允许）+ `warn` 日志：读不到参数时**不能**把 6 个主数据全锁死 |
| 未登录读策略接口 | 401 |

闸的位置：每个建单入口在 `req.validate()` 之后、`db().begin()` 之前 —— 被拒的请求**不开事务、
不取号**，不会留空号（curl 证据第 ⑥ 组：手工码提交不推进 `biz_seq`）。

## 前端行为

- `true`：编码栏可填（编辑态仍只读）、显示「生成编码」按钮、提示保持原文案（现状）。
- `false`：编码栏 `disabled`、按钮 **不渲染**、提示「编码由系统自动生成（管理员已关闭「允许手工指定编码」，保留空即可）」。
- 策略读不到（网络抖动 / 老后端无此路由）按 `true` 渲染 —— **fail-open**，真正的闸在后端。

## 复现步骤（99 本机，不碰 144/145）

```bash
cd /opt/cloudpivot
bash docs/evidence/20261108-code08-manual-code-switch/scripts/00_start_instance.sh start   # :8048 / 库 cloudpivot_code08c
bash docs/evidence/20261108-code08-manual-code-switch/scripts/01_curl.sh                   # 60 断言
bash docs/evidence/20261108-code08-manual-code-switch/scripts/02_db_assert.sh              # 38 断言
bash docs/evidence/20261108-code08-manual-code-switch/scripts/00_start_instance.sh stop
```

后端单测（真库）：

```bash
cd backend
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_code08c' \
  cargo test --lib -- business::erp::code_policy --test-threads=1
```

前端单测：`cd frontend && pnpm test:unit`（本卡文件 `tests/unit/code08-manual-code-switch.test.ts`）

## 落证清单（`raw/`）

| 文件 | 内容 |
|---|---|
| `00_summary.txt` | 60 条 curl 断言汇总（ALL PASS） |
| `00_login.json` / `00_probe.json` / `00_change_password.json` | 首登闸 → 改密 → 真 token（461 字符） |
| `00_config_page.json` | 参数行（迁移种入的真值：true / BOOLEAN / erp_master_data） |
| `00_config_put_false.json` / `00_config_put_true.json` | 管理端改参数（写后自动生效，不重启） |
| `10_policy_true.json` / `20_policy_false.json` / `40_policy_true_again.json` | 策略接口三次读数 true → false → true |
| `11_*`–`16_*` | 开关 true：六个主数据手工码 200 且原样保留 |
| `21_*`–`26_*` | 开关 false：六个主数据手工码 400 + 66054 + 文案 |
| `31_*`–`36_*` | 开关 false 时留空：六个主数据 200 自动取号（形态正确） |
| `41_*` / `42_*` | 改回 true：手工码恢复 200 |
| `50_policy_anon.json` | 无 token → 401 |
| `60_config_after.json` | 收尾时参数行（true / BOOLEAN / erp_master_data） |
| `01_migration_fresh.txt` / `01b_migration_idempotent.txt` | 空库首启 275 个迁移 / 二次启动 0 个 |
| `02_db_assert.txt` | 42 条库侧断言（ALL PASS） |
| `03_backend_unit_tests.txt` | 本卡 10 条用例（真库，无 skip） |
| `04_module_tests.txt` | 相关模块回归 116 passed / 0 failed |
| `raw_prev-debug-run/` | 调试期两轮落证（库被重复跑污染 / 二进制早于最后一次重构），正式证据用 `08d` |

## 测试结果汇总

| 层 | 结果 | 落证 |
|---|---|---|
| 真实 token curl（6 类 × 3 轮开关） | **60/60 PASS** | `raw/00_summary.txt` |
| 库侧断言（参数行 / 字典项 / 落库 / 不推进号段 / 无重复） | **42/42 PASS** | `raw/02_db_assert.txt` |
| 迁移幂等 | 空库首启 **275** 个 / 二次启动 **0** 个 | `raw/01_migration_fresh.txt`、`raw/01b_migration_idempotent.txt` |
| 本卡后端单测（真库 `ERP_DB_URL`） | **10 passed / 0 failed / 0 ignored** | `raw/03_backend_unit_tests.txt` |
| 相关模块回归（handler / customer_handler / dto / seq / asset_dto / expense_dto / error_code） | **116 passed / 0 failed** | `raw/04_module_tests.txt` |
| 后端全量 `cargo test --lib` | 3503 passed / **23 failed**（全部他卡/环境，本卡 0 条） | `raw/06_full_lib.txt` |
| 前端本卡单测 | **11/11 PASS**（+ CODE-05 换口径后 7/7） | `pnpm test:unit` |
| 前端全量单测 | 1088 passed / 3 failed（均他卡在飞改动） | 见 README §坑与偏差 8 |
| 前端 typecheck / build | exit 0 / exit 0（产物含 `code-policy` chunk） | `raw/05_checks.txt` |
| rustfmt / clippy | clean / 本卡文件 0 命中（2 个 error 是他卡 `tests/pflow1a.rs`/`pflow1b.rs`） | `raw/05_checks.txt` |

全量 23 条失败的逐条归因（**没有一条落在本卡路径**）：10 条 asset_linkage + 6 条 expense_linkage
在一次性库上 panic「库里没有部门 / 库里必须有一名带部门的用户」（该库只有 275 个 V 迁移、
`sys_dept` 0 行）；3 条 voucher_linkage 撞 `uk_flow_linkage_rule` 唯一键（同库上轮残留规则行）；
3 条 flow::linkage + 1 条 receivable_aging 是他卡在飞的联动 / 期间 WIP。失败日志里
`66054|系统已禁止手工指定编码|allow_manual` 命中数 = **0**。

## 坑与偏差（如实记录）

1. **更新系统参数是「整体替换」语义**：`ConfigSaveReq` 的 `config_key` / `config_type` 必填，且
   `build_model(&req, …)` 后整行 update —— curl 只发 `config_value` 会把 `category` / `select_value`
   写成 NULL（配置页会把它挪到「未分类」、开关控件退化成文本框）。证据脚本改成**整行提交**
   （值全部从库里读回来再发），与前端表单行为一致。**这不是本卡引入的问题**，但踩到了就要记下来。
2. **真库用例不许改全局参数**：最初的版本用「改 `sys_config` + 重载快照」验证两条分支，实测会
   污染同进程/并行进程的其它用例（`customer_handler` 4 条用例突然收到 400 + 66054）。改成
   `create_supplier_with_policy(…, allow_manual)` 把策略作为入参传进来：用例零副作用，
   「读参数」只做只读断言（`migration_seeds_switch_as_true`）。
3. **卡面写 `GET /api/system/config/public`**：仓里没有 `/api/system/config/*`，公共参数走
   `/api/support/config/public`（只下发 `public.` 前缀键），按键取值 `/api/support/config/value/{key}`
   要 `support:config:list` 权限。因此本卡在 ERP 侧新增
   `GET /api/business/erp/master-data/code-policy`（登录即可读、不挂权限码）：它是**界面策略**，
   挂任何单一主数据权限都会让「只有物料权限的人」拿不到状态；真正的闸在后端 66054。
4. **只拦建单、不拦改单**：6 个主数据的改单接口本来就不接受改编码（编码是档案身份），前端编辑态
   编码框也一直只读，因此没有额外加闸（写了也没有入口）。
5. **未碰计量单位**（卡面禁止）：单位编码仍手工填，本卡未改。
6. **未碰 144 生产**；未改任何已应用的迁移（新增 `V0342` 是首个空位，V0341 之后）。
7. **改了一条他卡的断言（不是放宽，是换口径）**：`frontend/tests/unit/code05-gen-code-button.test.ts`
   里「供应商/客户编码框必须 `:disabled="isEdit"`」钉的是 CODE-05 当时的字面量，本卡把禁用条件
   合并成 `manualCodeDisabled(isEdit, allowManualCode)`（编辑态 + 开关关闭都要禁用）。断言随之改成
   新表达式，**语义没变**（编辑态仍只读，由 `manualCodeDisabled(true, *) === true` 保证，
   code08 单测里有一条纯逻辑用例钉死）。
8. **前端全量单测的 3 条红是他卡在飞的改动**（与本卡无关，commit 前逐条归因）：
   `flow-biz-link.test.ts`（后端 FlowBizType 多出第 16 个字面量）、`erp-sales-order.test.ts`
   （payload 新增 `currency_code`）、`p307-awaiting-executor.test.ts`（`payable-logic` 暂缺
   `executePaymentConfirmText` 导出）。本卡相关的 18 条（code05 7 + code08 11）全绿。
