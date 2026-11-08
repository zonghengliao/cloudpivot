# CTR-02-F1 前端证据包（t_98ce6eab，2026-11-09）

合同四类全开的前端部分：四类可选 + 乙方服务商/出租方文案 + 履约「不适用」不渲染 0.00。

## 环境（真后端 + 真前端，均在本机隔离，未碰 :8023 / 145 / 144）

| 项 | 值 |
|---|---|
| 后端二进制 | `/root/ctr02f1-target/debug/cloudpivot`（= 后端卡 `t_07316875` commit `bbe9ba2` 编出来的包，`strings` 命中 `commitment_scope` 5 处） |
| 后端端口 | `127.0.0.1:18229`（`NIK_PROFILE=test`，profile=test + 64hex JWT secret + 64hex StorageMasterKey） |
| 数据库 | `cloudpivot_ctr02fe`（**全新空库**，首启自跑全量迁移 + 种子） |
| 前端 | `/opt/cloudpivot/frontend` 源码直接跑 `vite --port 5299`，`BACKEND_ORIGIN=http://127.0.0.1:18229`（**未 build、未动 dist**） |
| 浏览器 | 真实 Chromium（CDP），视口 1600×1000 |

## 断言总表（`asserts.txt`，全部来自真机 DOM 文本）

`PASS=22 / FAIL=0`：

- `SERVICE`（HT-20261109-000002）/ `LEASE`（HT-20261109-000003）详情：
  - 履约区只有灰字 `履约进度不适用（服务 / 租赁合同不走订单勾稽）`；
  - **没有** `¥0.00`、**没有**「已执行」、**没有**履约金额块（`contract-detail-commitment` 不存在）；
  - 「关联采购单」区块与「可被采购单引用」标记整块不渲染。
- `PURCHASE`（HT-20261109-000001）详情（回归）：已执行 `¥1,130.00` / 剩余 `¥8,870.00` / 未关闭 `1 张` /
  单号 `PO-20261109-000001`，且「关联采购单」表里就是这张单。

> 为什么 `PURCHASE` 的履约是真值：先用接口把这张合同送审（新建采购经理账号做审批人）→ 审批通过 `SIGNED`
> → 建一张 `purchase_type=CONTRACT` 的采购单 `PO-20261109-000001`（价税合计 1130）→ 合同详情 `committed_amount=1130`。
> 这条链同时证明「履约字段 = 后端现算」，前端没有自己算。

## 表单与列表

| 截图 | 内容 |
|---|---|
| `01-form-service-type.png` | 新建合同表单：类型下拉四类**全部可选**（无置灰）；已选「服务合同」→ 乙方 label = **乙方服务商**、下拉下小字 = `服务商 / 出租方在系统中登记为供应商`；下方「合同类型怎么选」四行引导常驻可见 |
| `02-form-service-filled.png` | 服务合同填完（名称 / 120000 / 乙方=北京诚信物业有限公司（SUP-WY）），乙方下拉里就是供应商清单 |
| `03-list-after-service.png` | 保存成功 → 列表出现 `HT-20261109-000002`，类型列 = 服务合同 |
| `04-form-lease-filled.png` | 租赁合同表单：乙方 label = **乙方出租方**，乙方 = 中关村写字楼业主委员会（SUP-ZL） |
| `05-list-four-types.png` | 列表三条：租赁合同 / 服务合同 / 采购合同，类型中文名都正确 |
| `10-detail-service-final.png` / `10-detail-lease-final.png` | 服务 / 租赁详情：履约情况 = 一行灰字，无 0.00 |
| `10-detail-purchase-final.png` | 采购详情：履约情况三格 + 单号，与关联采购单一致 |

列表 `类型` 筛选下拉也实测四类可选（`采购合同 / 销售合同 / 服务合同 / 租赁合同`，`t-is-disabled` 全 false）。

## 接口回执（机器可读）

```json
SERVICE  {"contract_type":"SERVICE","party_b_type":"SUPPLIER","commitment_scope":"NOT_APPLICABLE",
          "committed_amount":"0","remaining_amount":"120000.0000","open_order_count":0,"open_order_nos":[]}
LEASE    {"contract_type":"LEASE","party_b_type":"SUPPLIER","commitment_scope":"NOT_APPLICABLE",
          "committed_amount":"0","remaining_amount":"240000.0000","open_order_count":0,"open_order_nos":[]}
PURCHASE {"contract_type":"PURCHASE","party_b_type":"SUPPLIER","commitment_scope":"ORDER_LINKED",
          "committed_amount":"1130.0000","remaining_amount":"8870.0000","open_order_count":1,
          "open_order_nos":["PO-20261109-000001"]}
```

（`detail-service.json` / `detail-lease.json` / `contract-purchase-detail.json`）

## 门禁

| 命令 | 结果 |
|---|---|
| `pnpm typecheck`（`vue-tsc --noEmit`） | **EXIT=0**（`typecheck.txt`） |
| `tsx --test tests/unit/erp-contract.test.ts` | **14 passed / 0 failed**（`unit-contract.txt`，含新增 ⑯ 履约口径、⑰ 类型引导） |
| 全量 `pnpm test:unit` | 1238 passed / 3 failed；其中 `erp-contract.test.ts` 已全绿，另 2 条红是 **HEAD 既有**（同命令在 `git worktree` 干净 HEAD 上同样红，见交付说明） |

## 未做 / 边界

- 未改后端、未加迁移、未碰 `:8023` / 145 / 144 与生产 dist（本包内截图来自 5299 上的源码 dev server）。
- `SERVICE` / `LEASE` 的审批链未改动（后端卡已验），前端不按类型隐藏审批按钮。
- 隔离实例（18229）在证据跑完后停掉；库 `cloudpivot_ctr02fe` 保留便于复跑。
