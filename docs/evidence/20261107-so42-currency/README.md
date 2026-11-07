# SO-42 币种 / 汇率维护页 —— 证据

ARCH-7 §15.6（多币种）。卡面要求：币种维护页 + 汇率维护页（含按月批量录入）+ 销售单据币种选择器（汇率只读、锁定后不可改）+ 路由注册。

| 产物 | 路径 |
|------|------|
| 币种页 | `frontend/src/views/business/erp/currency/index.vue` |
| 汇率页（含按月批量录入） | `frontend/src/views/business/erp/exchange-rate/index.vue` |
| 前端 API 层（10 个接口） | `frontend/src/api/business/erp/currency.ts` |
| 后端 handler / model | `backend/src/business/erp/currency_admin_handler.rs` / `currency_admin_model.rs` |
| 迁移（菜单 + 10 个权限码 + 角色授权） | `backend/migrations/business/V0316__currency_menu.{up,down}.sql` |
| 路由 | `frontend/src/router/index.ts`（`business/erp/currency`、`business/erp/exchange-rate`） |
| 销售单据页改动 | `frontend/src/views/business/erp/sales-order/{index.vue,sales-order-query.ts}` + `api/business/erp/index.ts` |
| 证据脚本 | `scripts/verify/so42_currency_evidence.sh`（接口）、`scripts/verify/so42_ui_verify.mjs`（真机 UI） |

## 一、接口证据（`api_evidence.txt`，真实 token curl）

干净库 `cloudpivot_so42`（全新 `createdb`，启动时自动跑全量迁移）：

1. 迁移记账 `316|versioned|executed|currency_menu`；菜单 2 个页面节点 / 8 个按钮 / 10 个权限码承载 10 行；
   授权 `FINANCE_MANAGER=10 / SALES_SUPERVISOR=10 / SALES_CLERK=3`（销售员只读 + 预览）。
2. 币种：种子 4 币种齐全、本位币唯一（CNY）；小写码 `jpy` → 落 `JPY`；重复码 409；非法码 400。
3. 本位币切换：USD 立为本位币后库内 `is_base` 行数 = 1（唯一）；本位币停用 409（提示先换本位币）。
4. 汇率：新建 SPOT 成功；同键重复 409；汇率 = 0 → 400；自反（CNY→CNY）→ 400；不存在的币种 → 400。
5. 按月批量录入：`2026-11` → 生效日自动算成 `2026-11-30`，首次 `created 3 / updated 0`，
   第二次 `created 0 / updated 3`（upsert 覆盖、不改行）；非法月份 / 空批次 / 批内本位币一律 400。
6. 分页 / 编辑 / 删除：删两次分别是 200 / 404（物理删）。
7. 汇率预览（开单页用）：USD@2026-11-15 → `SPOT 7.05`（牌价生效日 2026-10-01）；CNY → `BASE 1`；
   EUR（只有 MONTH_END）→ `MISSING`（不臆造汇率，保存会被后端拒绝）。
8. 单据侧：USD 建单锁 `exchange_rate = 7.05`；把汇率表改成 9.99 后**老单仍 7.05（不回写）**；
   EUR 单（无 SPOT 汇率）→ 400。

## 二、真机 UI 证据（`ui_*.png` + `ui_evidence.txt`，vite dev + 真后端 + Chromium）

`node scripts/verify/so42_ui_verify.mjs`（需先起 vite dev :5188 反代真后端 :18098）：

- **① 币种管理**：4 个种子币种 + 本次新建的 THB 全部在列；「本位币」标记数 = 1（CNY）；
  弹窗新建 THB 后表格自动刷新出现（`ui-01…ui-03`）。
- **② 汇率管理**：列表显示币种对 / 汇率 / 类型 / 生效日期 / 备注 / 操作；
  「按月批量录入」弹窗按当前月回填已录值（EUR 7.8 / HKD 未录 / JPY 0.048 / USD 7.2），
  生效日自动 = 2026-11-30；把 USD 改 7.25 提交 → 列表出现 `7.25000000`（`ui-04…ui-06`）。
- **③ 销售订单**：新建弹窗有「币种」下拉；默认本位币汇率区显示 `1.00000000 本位币 CNY，恒 1`；
  选 USD 后汇率区显示 `9.99000000（1 USD = 9.99000000 CNY，牌价生效日 2026-11-07）`；
  **该区域 input 数量 = 0**（只读，没有任何输入框）；控制台 error = 0（`ui-07`）。

> 首轮真机跑出 1 条 console error「typeof value: undefined must be Array!」——
> 来自 `t-date-range-picker` 的 v-model 初值 `undefined`（功能不坏、控制台脏）。
> 已改成 `[]`，复跑 error = 0（`scripts/verify/so42_ui_console_probe.mjs` 分页定位）。

## 三、门禁

| 门禁 | 结果 |
|------|------|
| `pnpm typecheck` | 通过（无输出即无错） |
| `pnpm build` | 通过（`frontend/dist` 产物已生成；本机需 `NODE_OPTIONS=--max-old-space-size=8192`，默认堆会 OOM，属既有环境约束） |
| `cargo test --lib currency_admin` | 20/20 |
| `cargo test --lib business::erp::currency` | 34/34（含 SO-40 的 14 条纯逻辑） |
| `rustfmt --check`（本卡两文件） | 无 diff |
| `cargo clippy --lib` | 本卡文件 0 警告（仅剩既有 `sales_order_model::update_order too_many_arguments`，属 SO-40） |

## 四、口径说明（写在这里，避免下次重推一遍）

1. **币种不软删**：`biz_currency` 无 `deleted_at`（V0272 有意为之）。维护页只提供「启用 / 停用」，
   不提供删除 —— 停用后新单据不可选，历史单据与汇率按 `code` 引用照旧可查。
2. **本位币唯一且必须 ACTIVE**：`uk_biz_currency_single_base`（部分唯一索引）是库层硬保证；
   立本位币走同一事务「先清旧、再立新」；本位币停用被 409 挡住（`currency::base_currency_code`
   只认 `is_base AND status = 'ACTIVE'`）。
3. **汇率只存「外币 → 本位币」**：`ck_biz_exchange_rate_pair` 禁自反；反向由 `1 / rate` 推导，
   不落重复行（两行互为倒数必然因四舍五入对不上）。
4. **按月批量录入 = upsert**：同币对 / 同类型 / 同生效日（月末日）已存在则**改值不改行**；
   前端把「该月已录」回填进输入框，用户不改就是幂等重放。
5. **汇率预览与建单锁定同一把尺**：`quote` 复用 `currency::lock`，页面显示的 9.99 就是保存时落进
   `exchange_rate` 的值；外币没录汇率时 `source = MISSING` + `exchange_rate = 0`，
   前端明确提示「保存会被拒绝」，而不是显示 0 元汇率。
6. **单据侧汇率只读**：表单只给币种下拉，汇率是展示区（`[data-testid="so-form-rate"]` 里 0 个 input）；
   详情抽屉另显示「锁定汇率」与原币 / 本位币两个价税合计。
7. **权限**：`business:erp:currency:{list,create,update,base}` +
   `business:erp:fx-rate:{list,create,update,delete,batch,quote}`，与 V0316 的 `sys_menu.auth_code`
   逐字一致（`CURRENCY_ADMIN_PERMISSIONS` 常量 + 单测钉住 10 个码的形状）。
   开单选币种只需要 `currency:list` + `fx-rate:quote`，销售员（SALES_CLERK）已授。
