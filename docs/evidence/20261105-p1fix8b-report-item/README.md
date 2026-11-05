# P1-FIX-8B 报表项维护页（`report_item` 53 行从「只能改库」到可维护）

卡：`t_a100a01a` · 评审问题：「`report_item` 表 53 行但**无维护页** → 报表项只能改库」。

## 一、做了什么

| # | 交付 | 落点 |
|---|------|------|
| 1 | 后端维护面（7 个接口） | `backend/src/business/erp/report_item_admin.rs`（新，~1230 行） |
| 2 | `biz_report_item` 数据访问层 | `backend/src/business/erp/report_item_model.rs`（新，~350 行） |
| 3 | 迁移：页面菜单 + 2 个权限码 + 授权 | `backend/migrations/business/V0256__erp_report_item_admin.{up,down}.sql`（新） |
| 4 | 前端 API 契约 | `frontend/src/api/business/erp/report-item.ts`（新） |
| 5 | 维护页（列表 / 新增 / 编辑 / 排序 / 启停 / 取数预览） | `frontend/src/views/business/erp/report/report-item/index.vue`（新，~810 行） |
| 6 | 路由 | `frontend/src/router/index.ts`（+1 条，`BusinessErpReportItem`） |

接口一览（`/api` 前缀由框架注入）：

| 方法 | 路径 | 权限码 | 说明 |
|------|------|--------|------|
| GET | `/business/erp/report-item/list` | `business:erp:report-item:list` | 某张报表的全部报表项（含停用行）+ 引用关系 + 命中科目数 |
| GET | `/business/erp/report-item/catalog` | 同上 | 字典：报表 / 所属方 / 层级 / 取数模式语法 |
| POST | `/business/erp/report-item/preview` | 同上 | 取数预览：这条模式命中哪些科目 / 公式是否合法 |
| POST | `/business/erp/report-item` | `business:erp:report-item:manage` | 新增 |
| PUT | `/business/erp/report-item/{id}` | 同上 | 编辑（编码不可变） |
| PUT | `/business/erp/report-item/{id}/toggle` | 同上 | 启停 |
| PUT | `/business/erp/report-item/sort` | 同上 | 整表重排（传全量 id 顺序） |

## 二、为什么「改一个报表项，报表立即生效」

报表接口每次编制都现查 `biz_report_item`（`report::load_items_of` 按 `report_code + is_active=1`），
没有任何缓存层、也不写快照表 —— 所以维护页保存后**刷新报表就是新口径**，不需要发版、不需要清缓存。
证据见 `02-balance-sheet-after-rename.png`：页面上把「存货」改名后，资产负债表当场变成
「存货（P1-FIX-8B 真机改名）1,000.00」；改回原名后报表也立即复原。

## 三、验收逐条对照

| 卡面验收 | 结果 | 证据 |
|----------|------|------|
| 维护页完成 | ✅ 列表 / 新增 / 编辑 / 排序 / 启停 + 取数预览 + 页签（资产负债表 41 / 利润表 12）+ 关键字 / 状态 / 只看未命中筛选 | `01-report-item-list.png`、`page_check_round{1,2}.txt`（20 条 DOM 断言 FAIL 0） |
| 改一个报表项，报表立即生效（真机） | ✅ 页面改名 → 资产负债表立即显示新名与 1,000.00 → 改回即复原；HTTP 层另有「改取数模式 1405→1403：存货 0、1405 落未归类、资产总计仍 1000」 | `02-balance-sheet-after-rename.png`、`e2e_round{1,2}.txt`（40 条断言 FAIL 0） |
| `pnpm typecheck && pnpm build` 通过 | ✅ `vue-tsc --noEmit` EXIT 0；`vite build` EXIT 0（首次 4G 堆 OOM，提到 6G 后通过，见 `checks.txt`） | `checks.txt` |
| git commit | ✅ 见卡内汇报的 commit 号 | `git log` |

## 四、校验规则（写在这里，免得后人「顺手放宽」）

保存 / 排序前把「整张报表 + 本次改动」拼成候选集跑一遍**整表**校验，不合法直接 400 并回问题原文：

1. `formula` 只能引用**排在自己前面**的项（求值顺序 = `sort_order`）；
2. 引用了**已停用**的项 → 400（报表只加载 `is_active=1`，否则报表 500）；
3. 被别的公式引用的项**不能停用**（409 并点名引用者）；
4. 层级 2/3 必须二选一填取数模式或公式（层级 1 的纯标题行可以都不填）；
5. 取数模式语法：`1001,1002` / `2221`（含下级）/ `1*` `1%` / `@CATEGORY:类别`；
6. `item_code` 创建后不可改（它是公式的引用键）。

**一个必须记住的例外**：`UNMAPPED_ASSETS / UNMAPPED_LIABILITIES / UNMAPPED_EQUITY` 三行不是配置行，
是运行时按类别差额补出来的（`report::compute` 第 1.5 遍），所以公式引用它们必须放行、也不受排序影响
—— 否则 V0183/V0184 种的 `TOTAL_*` 合计行会被误判成死引用（首跑实测就踩到了这条）。

## 五、实测环境（隔离，不碰生产 144）

| 项 | 值 |
|----|-----|
| 库 | `cloudpivot_ri8`（99 本机，空库跑全量迁移 + bootstrap） |
| 夹具 | `biz_accounting_period` 2026-10 / 2026-11 + 一张平衡凭证：借 1405 库存商品 1000 / 贷 2202 应付账款 1000 |
| 后端实例 | `127.0.0.1:18160`（二进制 `/root/p1fix5c-target/debug/cloudpivot`） |
| 前端 | `vite 127.0.0.1:15160`（`BACKEND_ORIGIN=http://127.0.0.1:18160`） |
| 复现 | `bash scripts/verify/fix8b_report_item_db.sh recreate` → `fix8b_report_item_instance.sh start` → `fix8b_report_item_db.sh seed` → `python3 scripts/verify/fix8b_report_item_e2e.py` / `node scripts/verify/fix8b_report_item_page_check.mjs` |

两轮连跑：`e2e_round1.txt` ≡ `e2e_round2.txt`、`page_check_round1.txt` ≡ `page_check_round2.txt`（逐字一致）。

## 六、与卡面的差异 / 未做的事

1. **没有做「删除报表项」**：卡面列的是「列表/新增/编辑/排序/启停」，且报表项被公式引用时硬删会让报表
   500。停用可控且可回滚，删除留待需要时单独开卡。
2. `biz_report_item` **没有 `create_by/update_by` 列**（V0183 建表时就没给配置表加审计列），
   所以写入不带操作人；改配置的审计走 `audit_logs` 中间件。**没有**为这个去改表结构。
3. 生产 144 **未部署**（本卡只到 99 隔离实例）；上线需 `deploy.sh` 带 V0256 并清菜单缓存。
4. 迁移在「空库首启」时 `sys_role` 为空 → 授权是空操作（与 V0192/V0197/V0253 同口径）；
   bootstrap 之后重放本迁移会补齐超级管理员授权（实测重放：`SUPER_ADMIN` +1 行、其余 0 行，
   见 `db_menu_and_grants.txt`）。

## 七、证据清单

- `01-report-item-list.png` — 维护页整页（页签 / 统计 / 排序号 / 编码 / 名称 / 层级 / 所属方 / 取数来源+命中数 / 启用 / 编辑）
- `02-balance-sheet-after-rename.png` — 改名后资产负债表（新名字 + 1,000.00 + 勾稽平衡）
- `e2e_round1.txt` / `e2e_round2.txt` — HTTP 层 40 条断言（两轮逐字一致）
- `page_check_round1.txt` / `page_check_round2.txt` — 真机 DOM 20 条断言（两轮逐字一致）
- `checks.txt` — clippy / typecheck / build 结果
- `db_menu_and_grants.txt` — V0256 记账、3 个菜单节点、角色授权
