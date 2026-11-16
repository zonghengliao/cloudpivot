# M1-23 应收账龄分析页 — 证据

> 卡：`t_55590fbd`（M1-23【M1·财务】账龄：前端页面）
> 结论：新增独立报表页 `/business/erp/report/receivable-aging`，与「应付账龄分析」同层级；
> 后端**零改动**（读的是 M1-19/M1-20 已落地的 `GET /business/erp/receivable/aging`），
> 只新增 1 个页面权限码 + 菜单行（迁移 `V0418`）与前端页面 / 路由 / 单测。

## 1. 交付物

| 文件 | 作用 |
|------|------|
| `backend/app/migrations/business/V0418__receivable_aging_page.up.sql` | 页面码 `business:erp:report:receivable-aging` + 菜单页面行（父=财务管理，sort=52）+ 授权 + 祖先闭包 + 取数码补齐 + 8 项自检 |
| `backend/app/migrations/business/V0418__receivable_aging_page.down.sql` | 只删本卡种下的行（授权 → 菜单行 → 补授的两处），幂等 |
| `frontend/src/views/business/erp/report/receivable-aging/index.vue` | 页面（工具栏 / KPI / 客户汇总 / 单据明细 / 口径提示 / 空态 / 错误态） |
| `frontend/src/views/business/erp/report/receivable-aging/receivable-aging-logic.ts` | 纯逻辑（金额格式化、精确十进制比较、客户筛/排、明细收窄、文案、403 人话） |
| `frontend/src/router/index.ts` | 手工注册路由（漏了 `n08` 判红 = 点菜单 404） |
| `frontend/tests/unit/m123-receivable-aging-page.test.ts` | 27 条单测：纯逻辑 + 页面/迁移源码契约 |

## 2. 两个权限码（本卡最容易踩的一处）

| 码 | 管什么 | 承载行 |
|----|--------|--------|
| `business:erp:report:receivable-aging` | **页面**：侧栏显形 + 路由 `meta.permission`（后端零引用） | V0418 …1830（本卡） |
| `business:erp:receivable:aging` | **取数**：`GET /business/erp/receivable/aging` | V0288 …2882（既有） |

- 页面行的 `auth_code` **不能**直接写成取数码：那会让同一个码在 `sys_menu` 出现两行（授权树两个同名勾选框），
  V0412 的自检注释已把这种情形判为缺陷。
- 只授页面码的后果是「菜单点得进去、表格永远 403」，而 403 原文只有「无权限」四个字 ⇒
  迁移里**成对授权**（自检 ⑧「看得到 ⇒ 调得动」），页面里 **`requestErrorMessage` 点名缺哪个码**。
  这条已用真账号反证（见 §4.3）。

## 3. 迁移

```
V0418 页面行： id=…1830  path=/business/erp/report/receivable-aging
              component=business/erp/report/receivable-aging/index
              parent=…0e02（财务管理） sort_order=52  title=应收账龄分析  icon=NULL
授权：SUPER_ADMIN / FINANCE_MANAGER / FINANCE_DIRECTOR / GENERAL_MANAGER
      （与「应付账龄分析」V0255 同一套角色：两张账龄表对称，能看应付的岗位没理由看不到应收）
      不放 `TEST_UADMIN`：该码在 `sys_role` 里不存在，写上只会静默授 0 行，
      推送闸⑧ `scripts/verify/role_code_gate.py` 会判红（V0255 里的同名历史字面量属既有豁免）。
```

**祖先闭包**：`system/auth/handler.rs::build_menu_tree` 用 `utils::build_tree`，**父节点不在授权集合里的
子节点会被静默丢弃**。实测基线里「财务管理」（…0e02）只授给了 EMPLOYEE / FINANCE_MANAGER /
GENERAL_MANAGER，而 FINANCE_DIRECTOR 有「应付账龄分析」页面行却没有父目录 ⇒ 它侧栏里根本看不到应付账龄。
本卡的 ③-2 段按「谁持有页面行就补父目录」自愈（可重放），顺带修好了 FINANCE_DIRECTOR 的既有导航缺口。

**幂等**：`ON CONFLICT (id) DO NOTHING` + 授权 `NOT EXISTS` + 自愈段 + 自检块。
实测（演练库 `cp_m123_mig`）：首次 `INSERT 0 1 / 0 5 / 0 3 / 0 3`，重放全 `INSERT 0 0`，
自检 NOTICE `页面授权 5/5 角色`；`down` 连跑两次 `DELETE 5/1/2/3` → `DELETE 0/0/0/0`，库回到迁移前状态。

## 4. 门禁与实测（全部真实输出）

### 4.1 门禁

| 门禁 | 结果 |
|------|------|
| `cargo fmt --check` | exit 0 |
| `cargo clippy --lib -- -D warnings` | `Finished dev profile`，0 warning |
| 空库首启跑迁移 | 应用 **350** 个迁移，`V0418` 自检 NOTICE 通过（`applied_seq=338`） |
| `cargo test system::menu::tests::n08`（路由 ↔ 菜单路径） | `1 passed`（含新页 path） |
| `bash scripts/verify/menu_invariants.sh cp_m123_mig` | **8 条全绿**，菜单总数 **502**（400–520） |
| `pnpm typecheck` / `pnpm typecheck:unit` | exit 0 |
| `pnpm build` | 成功（`dist/` 产出，含新页 chunk） |
| `pnpm test:unit`（全量） | **1975 / 1975 pass** |

### 4.2 真接口（`m123_api_check.py`，33 条断言，**33 passed**）

在空库与灌入夹具后各跑一遍，全部通过，覆盖：
- 登录 → 菜单树里能找到 `/business/erp/report/receivable-aging`，title=应收账龄分析，祖先链含「财务管理」，component 逐字一致；
- `GET /receivable/aging`：`report_code=RECEIVABLE_AGING`、7 档（NOT_DUE…OVER_365）、summary 与 buckets 同长、hints 非空；
- 参数语义：`as_of=2026-11-30` → `as_of_source=QUERY`；只传 `period=2026-11` → `PERIOD`；`period=2022-12` → 200 且 0 客户；`as_of=2026-13-01` → 400；
- 导出：`GET …/aging/export` 200，响应体 `PK` 魔数、6 KB 级 xlsx。

### 4.3 真机（Playwright 浏览器，Vite dev + 真后端）

夹具（演练库，4 张应收单 / 3 家客户）：未收 63,800.49 / 逾期 51,800.49（基准日 2026-11-16）。

| 验收点 | 结果 |
|--------|------|
| 侧栏「财务管理 → 应收账龄分析」可点开，页头写「基准日 2026-12-31（期间末日）」 | ✅ |
| KPI：未收 63,800.49 / 逾期 63,800.49 / 100% / 3 家 4 张 / 范围应收 66,800.49、已收 3,000.00 | ✅ 与接口一致 |
| 表头按 `report.buckets` 渲染 7 档（未到期…365 天以上） | ✅ |
| 合计行 `合计（全表 3 家）` = 63,800.49，档位分摊 12,000 / 5,800 / 45,000.50 / 999.99 | ✅ |
| 客户筛选「远洋」→ 只剩上海远洋 1 家，**合计行仍 63,800.49**，并显示「已按『远洋』筛出 1 / 3 家；合计行仍是全表数（不按筛选重算）」 | ✅ |
| 排序「最长逾期降序」→ 合肥中辰 213 / 上海远洋 112 / 南京金鼎 55 | ✅ |
| 点客户行 → 明细收窄为 1 张，标题「客户「南京金鼎机电设备有限公司」 1 张（已筛出 1 / 共 4 张）」，出现「看全部客户」 | ✅ |
| 手改基准日 2026-11-16 + 查询 → 页头「（手动指定）」，逾期合计 51,800.49 / 占比 81.19% | ✅ 与接口同数 |
| 导出 → 下载 `应收账款账龄分析表_2026-12_截止2026-12-31.xlsx`（6,220 bytes） | ✅ |
| 空态：基准日 2020-01-01 → 「基准日 2020-01-01 没有未收清的应收单（这不是出错）」，KPI 全 0 | ✅ |
| **403 反证**：临时角色只授页面码、不授取数码（`M123_PAGE_ONLY`）→ 页面能进，红色提示「没有取数权限：本页需要 business:erp:receivable:aging。页面能打开但表格是空的，通常是「菜单授了、取数码没授」…」 | ✅ |

截图：`page-data.png`（有数据）、`page-detail-scoped.png`（点客户行收窄明细）。

## 5. 页面口径（写进代码，也钉在单测里）

- **前端不算账**：分档 / 汇总 / 逾期占比 / 最长逾期 / `total_unsettled` 全部由后端算；
  模板里没有 `reduce`、脚本里没有 `toFixed`（单测断言），金额只用十进制字符串格式化。
- **筛选不重算合计**：客户端加总 = 第二套账（明细有 1000 条截断、档位口径在库里），
  合计行永远取后端 `total_unsettled`，并显式标注「全表」。
- **客户排序用精确十进制比较**（`compareDecimalStrings`），不过 `Number()`：
  `1000000000000000.0001` 与 `…0002` 在 double 下相等，单据金额排序会静默乱序（单测钉死）。
- **分档不写死**：表头/客户行/合计行都按响应 `buckets[]` 渲染，后端加档位前端自动跟上；
  档位配色认不出的键回落灰色。
- **明细收窄只在渲染层**：`detailsForCustomer` 过滤已拿到的明细，不额外请求；
  重新出表后若选中客户已不在表里，自动复位并给「看全部客户」入口。
- **403 说人话**：区分「缺取数码」与其它错误，点名码值，避免用户去找运维。

## 6. 诚实缺口（没做，也不在本卡范围）

- **没有 Playwright 用例入库**：导出下载、403 提示、筛选/排序/明细收窄都在浏览器里真点过（§4.3），
  但没写成 `e2e/*.spec.ts`（与应付账龄页同口径 —— 那张页也没有下载类 e2e）。
- **销售主管（SALES_SUPERVISOR）不授本页**：它有取数码 `business:erp:receivable:aging`，
  继续通过「应收台账 → 账龄看板」看账龄；给销售开财务目录下的报表入口属管理决策，迁移不替人扩权。
- **应收台账页的「账龄看板」tab 保留**：台账顺手看一眼的场景仍在，两处读同一接口、同一口径，
  未做互相跳转（本卡「不夹带」）。
- **金额单元格是文本**（带千分位）：与应付账龄 / 三大报表导出同一口径，Excel 里不能直接 `SUM`。
