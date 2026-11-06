# SO-31 销售退货单前端页面 —— 证据包

> 卡：`t_f7b9e46c`【SO-31】退货单前端页面（ARCH-7 §15.2）
> 承接：`t_126909b0`（SO-30 退货过账，commit `09a7694`）/ `2c353b7`（SO-29 退货 API）/ `6f2303b`（SO-28 建表）
> 环境：本机（172.18.10.99）隔离库 `cloudpivot_so31` + 隔离后端 `127.0.0.1:18231` + vite dev `127.0.0.1:5191`

## 一、交付物

| 类型 | 文件 | 说明 |
|------|------|------|
| 新增 | `frontend/src/api/business/erp/sales-return.ts` | 5 个接口 + 全部类型（字段逐字抄 `sales_return_handler.rs`） |
| 新增 | `frontend/src/views/business/erp/sales-return/sales-return-query.ts` | 纯逻辑（状态机边表 / 按钮显隐 / 校验 / 金额公式 / 请求体） |
| 新增 | `frontend/src/views/business/erp/sales-return/index.vue` | 列表 + 从发货单新建 + 详情抽屉 + 5 个状态动作 + FlowProgress |
| 修改 | `frontend/src/router/index.ts` | 注册 `business/erp/sales-return`（`BusinessErpSalesReturn`） |
| 新增 | `frontend/tests/unit/erp-sales-return.test.ts` | 25 条单测（边表/标签逐条读后端源码比对） |
| 新增 | `backend/migrations/business/V0306__sales_return_menu_page.{up,down}.sql` | 补齐菜单 path/component/is_visible + 归位「销售管理」 |
| 修改 | `backend/src/business/erp/sales_return_handler.rs` | `ReturnRes` 增 `approval_instance_id`（FlowProgress 的读数，加法改动） |
| 新增 | `scripts/verify/so31_return_ui_e2e.sh` | 真实 token 走完页面调的 5 个接口（56 断言） |

## 二、验收（全部真跑，日志在本目录）

| 验收项 | 结果 | 证据 |
|--------|------|------|
| `pnpm typecheck` | exit 0 | `typecheck.txt` |
| `pnpm build` | exit 0，364 个 gz 产物；页面 chunk `index-CuIcQMb0.js` 内含 `从发货单新建退货单` / `btn-return-post-confirm` | `build.txt`、`raw/dist_chunk_grep.txt` |
| `pnpm test:unit` | 1012 passed / 0 failed（含本卡新增 25 条） | `unit_test.txt` |
| `cargo test --lib` | 2888 passed / 0 failed | `cargo_test.txt` |
| 真实 token 接口全流程 | 56 / 56 全绿 | `e2e_curl.txt` + `raw/*.json` |
| 真机 UI 全流程（Playwright） | 53 / 53 全绿 + 7 张截图 | `ui_playwright.txt`、`0*.png` |
| 空库迁移重放 | 233 个迁移 APPLY_OK，V0306 打出 NOTICE；`applied_via` 非法 = 0 | `raw/migration_log.txt` |
| V0306 down → up → 二次 up | down 还原（path/component=NULL, visible=f, parent=NULL, sort=79）→ up → 二次 up「改 path 0 行」 | `v0306_drill.txt` |

> ⚠️ `pnpm build` 在本机需要 `NODE_OPTIONS=--max-old-space-size=4096`：会话环境预置
> `--max-old-space-size=1536`，而本机同时有别人的 `rustc` 在编（实测吃 6GB），直接跑会
> `FATAL ERROR: Reached heap limit`。这是环境内存，不是代码问题（`typecheck` 单独跑 exit 0）。

## 三、页面能力（对应卡面四条要求）

1. **列表**：`return_no` / 客户 / 退货日期 / 类型 / 金额(价税合计) / 状态 + 原发货单 + 操作，
   服务端分页（20/页，`showJumper`），3 个过滤（单号模糊 / 状态精确 / 客户），
   「已筛选」标记与空态文案区分「没数据」与「筛出来没有」。
2. **从原发货单生成**：下拉只列 `status=POSTED` 的发货单 → 调
   `GET /sales-return/returnable/{shipment_id}` 带出可退明细（已发 / 已退 / 可退三列），
   数量默认「可退量」、可改小（部分退）、可删行；仓库按原发货单仓库预填（可改），
   弹窗底部实时预览「不含税 + 税 = 价税合计」（与后端 `line_amounts` 同公式）。
3. **提交 / 作废按状态显隐**：按钮显隐唯一读 `RETURN_EDGES`（= 后端 `SALES_RETURN_EDGES` 7 条边）：
   `DRAFT`→提交、`SUBMITTED`→批准、`APPROVED`→待过账、`PENDING_POST`→过账、三个非终态→作废。
   已过账行只剩「详情」；待过账**没有作废**（边表里没有那条边，前端不造）。
   `SUBMITTED / APPROVED` 作废必须填原因（后端 400 的前端同款闸）。
4. **审批进度用 `FlowProgress.vue`**：详情抽屉按共享口径
   `loadApprovalProgress(detail.approval_instance_id, getFlowInstance)` → `hasApprovalProgress` 决定是否渲染。
   **现状必须说清**：退货单尚未接入流程引擎（`change_return_status` 不建实例，
   `biz_sales_return.approval_instance_id` 恒空），所以这一块现在**不渲染**
   —— 这是 `views/business/erp/shared/approval-progress.ts` 的第一条硬口径（无实例 id 就不画空标题）。
   后端 DTO 已由本卡补下发该字段，引擎接上后前端零改动自动显示。
   替代可视进度：详情里的 5 段单据状态线（草稿 → 已提交 → 已批准 → 待过账 → 已过账，作废单只点亮草稿）。
5. **路由注册**：`business/erp/sales-return` + `meta.permission='business:erp:sales-return:list'`；
   菜单由 V0306 补齐（`/business/erp/sales-return`、component `business/erp/sales-return/index`、
   `is_visible=TRUE`、parent=「销售管理」、sort 70），侧边栏实测可见（截图 `01-list.png` 左侧）。

## 四、真机 UI 走查（截图）

| 截图 | 内容 |
|------|------|
| `01-list.png` | 列表 8 列齐全，已过账行只剩「详情」，菜单「销售退货单」在「销售管理」下 |
| `02-create-dialog.png` | 从发货单新建：带出可退明细（已发 7 / 已退 3 / 可退 4 与 5），预估价税合计 1,582.00 |
| `03-detail-draft.png` | 草稿详情：状态线 + 描述 11 项 + 明细 2 行；无「审批进度」块（未接引擎） |
| `04-detail-pending-post.png` | 待过账详情：只剩「过账」（没有作废按钮） |
| `05-post-confirm.png` | 过账确认弹窗：五个真实效果 + 「不可撤销」 |
| `06-detail-posted.png` | 已过账：回链（入库单 / 应收 / 红票）+ 动作按钮全消失 |
| `07-list-after-post.png` | 回到列表：2 张单均为已过账 |
| `09-detail-draft-buttons.png` | 抽屉头部「提交 / 作废 / 关闭」三按钮实测可见、均在 960px 抽屉内容区内（无溢出/换行） |

DOM 硬数字（`09` 那次量测）：抽屉 body `x=640 w=960`，按钮 `提交 x=1432 w=40`、
`作废 x=1488`、`关闭 x=1544`，三者 `insideBody=true`。

## 五、接口实测（`e2e_curl.txt` 摘）

```
列表 page/no-items → 200，行内含 approval_instance_id（新字段）、customer_name、shipment_no 均已补齐
可退明细 returnable/SO30-SH-A → 200，2 行（remaining 7 / 5，price 100 / 200，tax 13%）
建单 POST → 200 DRAFT SR-20261107-000001，金额 300.0000、税 39.0000（= 3×100 与 ×13%）
超退 400：items[0].qty = 9999 超过可退量 4.0000（已发 7.0000 − 已退 3.0000）：超退被拒绝
提交 200 SUBMITTED → 重复提交 409 → 批准 200 → 待过账 200 → 待过账作废 409（边表无此边）
过账 200 POSTED，回填 stock_in_id / receivable_id；重复过账 409
三本账：stock_in=IN-20261107-000001 qty=3.0000 status=POSTED；
        txn=1 qty_sum=3.0000 after_sum=103.0000（原 100 → 103）；cost_book=103.0000/6180.0000
菜单：/business/erp/sales-return | business/erp/sales-return/index | visible=t | parent=销售管理 | sort=70
迁移：versioned/executed；applied_via 非法行数 0
```

## 六、复现

```bash
cd /opt/cloudpivot
SO30_DB=cloudpivot_so31 bash scripts/verify/so30_db_setup.sh
SO30_DB=cloudpivot_so31 SO30_PORT=18231 SO30_LOG=/root/so31_server.log \
SO30_PIDF=/root/so31_server.pid bash scripts/verify/so30_instance.sh up
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so31 -f scripts/verify/so30_posting_fixture.sql
SO30_PORT=18231 SO30_DB=cloudpivot_so31 bash scripts/verify/so31_return_ui_e2e.sh     # 接口层 56 断言
# 真机 UI（vite dev 指 18231，脚本在 workspace 外，不进仓库）
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18231 ./node_modules/.bin/vite --port 5191 --strictPort
node <scratch>/so31-verify/verify.mjs                                                 # UI 53 断言
```

## 七、边界与残留（不夸大）

- **退货单未接流程引擎**：`SUBMITTED → APPROVED` 目前是人工点状态机，没有审批实例、没有待办/抄送。
  接入需要后端卡（`sales_approval_bridge` 同款：发文流程 + `change_return_status` 建实例 + 回写
  `approval_instance_id`）。本卡只把前端接线做好（DTO 字段已下发），接上即显示。
- **未授角色**：V0304/V0306 都没写 `sys_role_menu`（授谁看退货单是管理决策），现状只有 SUPER_ADMIN 可见可用；
  实测就是用 admin 跑的。
- **一物一码物料退货**仍被后端 400 显式拒绝（SO-30 残留，需序列号状态流转独立卡）。
- **`pnpm build` 产物落 `frontend/dist`**：卡面要求就是 `pnpm build`，本机这棵树相对 HEAD 只有本卡文件
  （`git status` 可查），因此没有把别人的半成品打进产物；生产 dist 在 172.18.10.144，本卡未发布。
