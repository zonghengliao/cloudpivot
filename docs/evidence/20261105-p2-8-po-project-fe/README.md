# P2-8-F1 采购单「所属项目」前端验收证据

卡：`t_d3c85f12`（父卡 `t_b87398c8`，后端契约已冻结：V0237 + `project_id` / `project_name` / 建单·改单 `project_id` / 列表筛参）
日期：2026-11-05（CST）

## 一、验收环境（隔离实例，未碰生产）

| 组件 | 取值 |
|---|---|
| 后端二进制 | `/root/p28-target/debug/cloudpivot`（父卡在 `797cc1b` 的隔离 clone `/root/p28-tree` 里编的，带 V0237 + 新 DTO） |
| 后端监听 | `127.0.0.1:18157`（`NIK_PROFILE=test`，随机 JWT secret + 64 位 hex 存储主密钥） |
| 库 | `cloudpivot_p28fe`（`createdb -T cloudpivot_p28b` 克隆，`biz_purchase_order.project_id` 列已存在） |
| 前端 | `vite` dev server `127.0.0.1:5183`，`BACKEND_ORIGIN=http://127.0.0.1:18157`（**源码即产物**，没有 build、没有碰 `/opt/cloudpivot/frontend/dist`） |
| 账号 | `admin`（克隆库 admin 口令对齐 `cloudpivot_test`，`must_change_password=false`） |

生产站点、生产库、生产 dist 一律未触碰。

## 二、改动清单

| 文件 | 改动 |
|---|---|
| `frontend/src/api/business/erp/index.ts` | `PurchaseOrderRow` 加 `project_id` / `project_name`；`CreatePurchaseOrderReq` 加 `project_id?`；列表查询参数加 `project_id?`（只有类型，本卡不做筛选条 UI） |
| `frontend/src/views/business/erp/purchase/purchase-logic.ts` | `PurchaseFormState.project_id`、`emptyPurchaseForm()`（`''`）、`detailToForm()`（`project_id ?? ''`）、`buildPurchasePayload()`（**恒发** `project_id: form.project_id \|\| null`）；新增 `projectDisplayName()` / `projectOptionLabel()` / `projectSelectOptions()` / `DELETED_PROJECT_TEXT` / `NO_PROJECT_PLACEHOLDER` |
| `frontend/src/views/business/erp/purchase/index.vue` | 表单「所属项目」下拉（`clearable` + `filterable`，`data-testid=purchase-project`，与供应商/收货仓库同一排）；列表「项目」列（供应商之后，宽 160，`ellipsis`）；详情抽屉「所属项目」行（`data-testid=purchase-detail-project`）；`loadBasics()` 拉 `pageProjects({page:1,page_size:100}).catch(() => null)`；编辑回填时记 `editingProjectName` 供下拉兜底项用 |
| `frontend/tests/unit/p2-8-po-project.test.ts` | 19 条单测（状态/请求体/显示回退/下拉兜底/源码契约） |

**没有做**：后端一行未动、迁移未动、未做「按项目筛选」筛选条、未改供应商名口径（属 `t_f5fbd004`）。`project_id` **没有**进 `MEMORY_FIELDS`（单测钉住）。

## 三、验收结论（逐条对卡面）

| 验收项 | 结果 | 证据 |
|---|---|---|
| `pnpm typecheck` 0 error | ✅ 通过（`vue-tsc --noEmit`，无输出即 0 error） | `typecheck.txt` |
| 新增单测全绿 | ✅ 19/19；全量 `pnpm test:unit` 759/759（本卡前 740） | `unit-test-p2-8.txt`、`unit-test-all-summary.txt` |
| 建单 → 选项目 → 保存 → 列表显示项目名 → 详情显示项目名 | ✅ `PO-20261105-000099`：列表「项目」列 = `P28FE 采购挂项目`，详情「所属项目」= 同名 | `ui-flow.txt`、`01`/`02`/`03` 截图 |
| 编辑清空 → 保存 → 列表/详情回到「—」 | ✅ `PUT` 请求体 `project_id: null`（**键存在**）、列表与详情均 `—`、接口回读 `project_id=null / project_name=null` | `ui-flow.txt` ⑥⑦⑧、`04`/`05` 截图、`api-page-rows.json` |
| 建单请求体带项目 | ✅ `POST` 体 `project_id = 01a1a7fe-…bfbf`（真 uuid） | `ui-flow.txt` ③ |
| 归档项目不被筛掉 | ✅ 下拉里有 `P28FE 归档项目（P28FE-PO-OLD）（已归档）` | `ui-flow.txt` ② |
| 「项目已删除」回退（补充验证） | ✅ 软删项目后：列表/详情显示 `（已删除项目）`（不落 uuid），编辑弹窗下拉显示 `（已删除项目）（当前所属项目）` 而不是静默变空 | `ui-flow-deleted.txt`、`06`/`07`/`08` 截图、`api-page-rows.json`（`000100`：`project_id` 仍为 uuid、`project_name` 为 `null`） |

## 四、顺手修的一个布局回归（同文件、有实测数字）

加「项目」列后整表宽度从 1330 涨到 1490，而 `t-table__content` 在 1600 视口只有 **1288px**，
`操作` 列是 `fixed: 'right'` —— 多出来的 202px 被它盖住，**最早被盖的正是「状态」列**（整列看不见）。
所以同一笔改动里把相邻几列按实际文本宽度收窄：合同号 170→130、供应商 160→132、类型 100→88、
日期 100→76、金额 150→112、状态 110→96、勾选 50→44。**采购单号保持 250**（19 位 mono + 口径徽标，
压窄就换行，实测 200 时会折成两行）。

实测（`table-widths.txt`，1600×1000 视口）：

- 加列但未收窄：`client=1288 scroll=1490`，溢出 202 → 状态列被操作列完全遮挡；
- 收窄后：`client=1288 scroll=1328`，溢出 40 → 状态列「草稿 / 已收货」正常可读（加列前各列宽相加 = 1330，溢出 42，现与它持平）；
- 1440 / 1280 视口下仍会横向滚动（10 列 + 固定操作列的结构限制，加列前同样如此），滚动即可看到右侧列。

## 五、发现 / 已知边界（都不在本卡范围，未擅自改）

1. **项目被软删后，这张单再保存会被后端拒收**：`400 + 66002「project_id 项目不存在或已删除」`（`ui-flow-deleted.txt` ⑧ 实测）。
   前端已按卡面要求「钉住当前值不清空」，但用户改别的字段也保存不了 —— 需要产品/后端拍「已删项目的单是否允许改单时保留原归属」，
   建议另开卡（前端可配合：给已删项目的单据做只读提示或「改为未归集」的显式动作）。
2. **项目下拉受项目可见性裁剪**（`/business/project/page`）：普通采购员看不到自己没参与的项目。前端已用「当前所属项目」兜底项
   保证编辑老单不会静默清空，但「可选任意项目」需要只读全量选择器接口（父卡 known_limits 已记）。
3. 页面加载时那条 `401` 来自 `/api/business/notice/stream`（全站既有缺陷，`EventSource` 带不了 Authorization），与本卡无关。

## 六、证据文件

```
typecheck.txt                    pnpm typecheck（vue-tsc --noEmit，0 error）
unit-test-p2-8.txt               新增 19 条单测的完整 TAP 输出（19 pass / 0 fail）
unit-test-all-summary.txt        pnpm test:unit 全量汇总（759 pass / 0 fail）
table-widths.txt                 1600/1440/1280 视口下各列实际宽度与溢出量
ui-flow.txt                      主流程真机日志（建单→列表→详情→编辑清空→回读）
ui-flow-deleted.txt              项目软删后的显示回退 + 编辑兜底项 + 保存被拒回执
api-page-rows.json               接口回读：000099（清空后 null）/ 000100（项目已删，id 在名字 null）
01-form-project-selected.png     表单：所属项目已选（与供应商/收货仓库同排）
02-list-project-column.png       列表：项目列显示项目名（状态列可用）
03-detail-project.png            详情：所属项目显示项目名
04-list-project-cleared.png      清空后列表：项目列「—」
05-detail-project-cleared.png    清空后详情：所属项目「—」
06-list-deleted-project.png      项目被软删：列表显示「（已删除项目）」
07-detail-deleted-project.png    项目被软删：详情显示「（已删除项目）」
08-form-deleted-project-fallback.png  项目被软删：编辑弹窗下拉仍是「（已删除项目）（当前所属项目）」
verify.mjs / verify-deleted.mjs  两份 Playwright 脚本（真点击、真接口，无 route 拦截）
```
