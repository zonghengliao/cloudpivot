# MENU-FIX-1：菜单 title 改短 + sort_order 去重（V0335）

- 卡：`t_95bc00b0` ｜ 分支：main ｜ commit：`0872b6a`（本次迁移）+ 证据提交
- 迁移：`backend/migrations/business/V0335__menu_title_shorten_and_sort.up.sql`（235 行）
  / `.down.sql`（117 行）
- 实测环境：145 测试（`cloudpivot_dev`，systemd `cloudpivot-test`，debug 二进制）
  ＋ 99 全新库 `cloudpivot_menufix`（纯迁移链，用于验证「新机器也不乱」）

## 0. 结论先行

| 验收项 | 结果 |
|---|---|
| ① `menu_type IN (1,2)` 且 `length(title) > 12` | 145：**25 → 0** ｜ 全新库：**0** |
| ② 同父节点 `menu_type=2` 的 `sort_order` 重复组 | 145：**5 → 0** ｜ 全新库：**0** |
| ③ `menu_invariants.sh` 5 条断言 | 145：**全绿 rc=0（423 条）** ｜ 全新库：**全绿 rc=0（423 条）** |
| ④ 前端三模块实际显示 | 25 条全部短名、单行；刷新 3 次顺序**完全一致** |
| 幂等 | 二次执行 up.sql：**31 条 UPDATE 全 0 行** |

## 1. 重复菜单排查（卡面要求）

结论：**没有任何真正的重复菜单**。三条 path 各有多行，是「目录行(menu_type=1) + 页面行(menu_type=2)」的父子对，
共用同一 path 串属既有结构（不变量 ④ 只约束 `menu_type=2`，此处无冲突）。

145 实测（改前）：

```sql
SELECT path, count(*) AS cnt, string_agg(menu_type::text || ':' || name, ' | ' ORDER BY menu_type)
  FROM sys_menu WHERE deleted_at IS NULL AND menu_type = 2
 GROUP BY path HAVING count(*) > 1;
-- 只有 1 行：path 为空的两条历史占位行（收款退款单 / 价格查询），不是下面的三条
```

| path | 行数 | 组成 | 是否重复 |
|---|---|---|---|
| `/business/erp/expense` | 2 | `费用报销`(mt=1，财务下目录) + `我的报销`(mt=2，其子页) | 否 |
| `/business/erp/asset` | 2 | `固定资产`(mt=1，财务下目录) + `资产卡片`(mt=2，其子页) | 否 |
| `/business/erp/budget` | 2 | `预算管理`(mt=1，财务下目录) + `预算编制`(mt=2，其子页) | 否 |
| `/business/erp/expense/pay` | 1 | `待付款`(mt=2) — 卡面注里把它算进 `/business/erp/expense` 是笔误 | — |

卡面注「`/business/erp/expense` 有 3 条」实测为 **2 条**（第 3 条是 `.../expense/pay`）。
`SELECT parent_id, path, menu_type, count(*) ... HAVING count(*) > 1` 在 `menu_type=2` 上只命中
两条 path 为空的占位行（`收款退款单` / `价格查询`，互不相同，非重复）。**本卡未删任何菜单。**

## 2. 22 条 title 改前/改后（145 实测，含顺带修的 3 条）

```sql
-- 改前
SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND menu_type IN (1,2) AND length(title) > 12;
--  25
```

| 原字数 | 原 title（截前 24 字） | 新字数 | 新 title | path |
|---|---|---|---|---|
| 84 | 收款退款单：RR-yyyyMMdd-000001 …（占位行） | 5 | 收款退款单 | (空) |
| 79 | 取价解析（5 级确定性取价：客户协议价 → …（占位行） | 4 | 价格查询 | (空) |
| 47 | 销售目标维护 + 达成率看板（按订单金额 / 按… | 4 | 销售目标 | /business/erp/sales-target |
| 38 | 即时 / 月末汇率维护 + 按月批量录入（ARCH… | 4 | 汇率管理 | /business/erp/exchange-rate |
| 37 | 客户协议价 + 阶梯/促销价维护（取价链见 AR… | 4 | 客户价格 | /business/erp/customer-price |
| 36 | 采购退货：选入库单带明细 → 审批 → 库存回冲… | 4 | 采购退货 | /business/erp/purchase-return |
| 34 | 询价 → 多家报价 → 比价 → 定标 → 转采… | 3 | 询比价 | /business/erp/price-inquiry |
| 34 | 币种主数据 + 本位币标记（唯一；ARCH-7 … | 4 | 币种管理 | /business/erp/currency |
| 29 | 报表项维护（报表项 ↔ 科目映射 / 取数公式… | 5 | 报表项维护 | /business/erp/report/report-item |
| 28 | 部门提需求 → 审批 → 转采购订单（支持多单合… | 4 | 采购申请 | /business/erp/purchase-requisition |
| 27 | 跨单据联动规则（源单据 + 触发状态 → 白名单… | 4 | 联动规则 | /flow/linkage |
| 25 | 费用类型（科目 / 可抵扣 / 受预算 / 上限 | 4 | 费用类型 | /business/erp/expense-type |
| 23 | 我的报销（新建 / 提交 / 撤回 / 作废） | 4 | 我的报销 | /business/erp/expense |
| 22 | 费用报销（报销单 / 待付款 / 费用类型） | 4 | 费用报销 | /business/erp/expense |
| 20 | 预算管理（编制 / 执行 / 科目映射） | 4 | 预算管理 | /business/erp/budget |
| 20 | 折旧计提（月度批次 / 凭证 / 红冲） | 4 | 折旧计提 | /business/erp/depreciation |
| 20 | 审批流程定义（编排 / 发布 / 版本） | 6 | 审批流程定义 | /flow/def |
| 19 | 预算执行（已用 / 可用 / 执行率） | 4 | 预算执行 | /business/erp/budget/report |
| 19 | 资产分类（年限 / 残值率 / 科目） | 4 | 资产分类 | /business/erp/asset-category |
| 18 | 预算编制（部门 × 科目 × 期间） | 4 | 预算编制 | /business/erp/budget |
| 18 | 资产卡片（建卡 / 转移 / 处置） | 4 | 资产卡片 | /business/erp/asset |
| 18 | 固定资产（卡片 / 分类 / 折旧） | 4 | 固定资产 | /business/erp/asset |
| 17 | 合同管理（采购/销售/服务/租赁） | 4 | 合同管理 | /business/erp/contract |
| 17 | 预算科目映射（采购 → 预算科目） | 6 | 预算科目映射 | /business/erp/budget/mapping |
| 15 | 待付款（已审批未付清的报销单） | 3 | 待付款 | /business/erp/expense/pay |

`name` 字段**一律未动**（菜单唯一业务键，权限码/路由按它对齐）。注意 `销售目标与预测` 的 name 与
新 title `销售目标` 不同，属预期——只改展示层。

## 3. sort_order 去重（4 组 + 1 组只在迁移链里出现）

改前（145）：

```
 /business/erp/finance-mgmt  | 47 | 现金流量表 ;; 应付账龄分析
 /business/erp/purchase-mgmt | 40 | 供应商 ;; 采购订单
 /business/erp/sales-mgmt    | 70 | 价格管理 ;; 销售退货单
 /business/erp/sales-mgmt    | 80 | 佣金规则 ;; 币种管理
 /business/erp/sales-mgmt    | 90 | 佣金结算 ;; 汇率管理
```

改后：

| 父目录 | 菜单 | 原 | 现 |
|---|---|---|---|
| finance-mgmt | 应付账龄分析 | 47 | **48**（紧跟报表组 45/46/47） |
| finance-mgmt | 报表项维护 | 48 | **49**（维护类垫在报表后，让位给 48） |
| purchase-mgmt | 采购订单 | 40 | **10**（采购单据排第一，与生产一致） |
| sales-mgmt | 销售退货单 | 70 | **72** |
| sales-mgmt | 币种管理 | 80 | **82** |
| sales-mgmt | 汇率管理 | 90 | **92** |

卡面给的「财务 48 撞报表项维护(48)」处理：**报表项维护 49**（不是 50），既避撞又保持
「报表 45→48 → 维护 49」的连续编号，不留空洞。

## 4. 顺带收敛的 3 处「生产手工改过、迁移链没写」分叉

跑全新库（纯迁移链）时自检红了两次，暴露的是**生产当年手工改库、迁移链里没补迁移**：

| 项 | 迁移链 | 生产(144) | 处置 |
|---|---|---|---|
| `/flow/def` 流程定义 title | 20 字（V0121） | 6 字「审批流程定义」 | 迁移里补 UPDATE，目标值取生产现值 |
| `/business/erp/purchase-order` 采购订单 sort | 40（V0141，与供应商撞） | 10 | 迁移里补 UPDATE，目标值取生产现值 |
| 两条 path 为空的占位行 title | 79 / 84 字 | 同（也是长的） | 一并收短 |

生产侧执行这三条是 **0 行改动**（幂等判据生效），新环境从此与生产一致——这正是
`docs/编码规范.md` 里「手工改过库必须补幂等迁移」的那条规矩。

## 5. 部署必读：改完必须清菜单树缓存（否则侧栏最长 1 小时还是旧名）

菜单树在 Redis 里缓存（`backend/src/system/menu/model.rs`，TTL `CACHE_HOT_QUERY_TTL_SECS = 3600`），
键名**带 profile 前缀**，实际键是：

```
cloudpivot:menu:tree:all      # 不是 menu:tree:all
```

迁移是直接改库，走不到 handler 的 `invalidate_tree_cache()`，所以**不清缓存就会出现「库里已短、页面上还长」**。
145 实测证据（清缓存前/后，同一个 token 打 `/api/system/menu/tree`）：

```
清除前：API 树里 >12 字菜单 25 条
redis-cli DEL cloudpivot:menu:tree:all   -> 1
清除后：API 树里 >12 字菜单 0 条
```

标准动作（每次涉及 sys_menu 的数据迁移都适用）：

```bash
redis-cli DEL cloudpivot:menu:tree:all      # 按实际前缀/实例调整
```

生产（144）上线本迁移时同样要执行这一步，然后刷新页面确认。

## 6. 迁移自检（写在 SQL 里，任一不满足直接 RAISE，宁可失败也不留半条）

1. ① 25 条点名行 `title` 必须等于目标值（行不存在则跳过，兼容新环境无占位行）；
2. ② 全库 `menu_type IN (1,2)` 不得有 `length(title) > 12`；
3. ③ 同父节点 `menu_type=2` 不得有重复 `sort_order`。

全新库（`cloudpivot_menufix`，423 条菜单）跑通即为「新环境也不乱」的证据：
`schema_migrations` versioned 257 / seed 11、失败 0、最大版本 335；`menu_invariants.sh` 5 条全绿。

## 7. 未做 / 遗留（本卡范围外，已在汇报里点名）

1. **顶级菜单 `sort_order=75` 撞车**：`合同管理`(mt=2) 与 `通用底座`(mt=1) 同为 75、父均为 NULL。
   因是「目录 vs 页面」不属卡面 ② 口径（`menu_type=2` 分组已 0 行），且改顶级顺序会动整个侧栏观感，
   本卡不动，建议另开卡。
2. **结构基线过期**：`scripts/verify/baseline/menu_structure_baseline.txt` 早于 P2-01/02/03，
   全新库对 `--strict` 有 15 处「仅 A」差异（price-inquiry / purchase-requisition / purchase-return 的按钮行）。
   与本卡无关（本迁移只写 `title` / `sort_order`，不碰 path/parent/menu_type），需另行重生成基线。
3. 生产 144 尚未部署本迁移（本卡按卡面只做 145 + 新库验证）。上线步骤：`git pull` → 重启
   （迁移在启动时执行）→ `redis-cli DEL cloudpivot:menu:tree:all` → 刷新页面。
