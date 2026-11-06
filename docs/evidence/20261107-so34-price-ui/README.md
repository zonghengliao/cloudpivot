# SO-34 价格管理前端（协议价维护 + 订单自动带价）交付证据

日期：2026-11-07 · 卡：`t_1a84c480` · ARCH-7 §15.3

## 1. 交付内容

| 层 | 文件 | 说明 |
|----|------|------|
| 后端 · 数据层 | `backend/src/business/erp/price_admin_model.rs`（新） | 协议价 / 档位两表 CRUD + 唯一键查询 + 分页（`page_of` → `scope::paginate_unscoped`） |
| 后端 · 接口层 | `backend/src/business/erp/price_admin_handler.rs`（新） | 8 个接口（4 协议价 + 4 档位）+ DTO 校验 + 24 个单测 |
| 后端 · 注册 | `backend/src/business/erp/mod.rs`（改 2 行） | `pub mod price_admin_{model,handler};` |
| 迁移 | `backend/migrations/business/V0308__customer_price_menu.{up,down}.sql`（新） | 页面菜单 + 7 个按钮 + 8 个权限码 + 角色授权 |
| 前端 · API | `frontend/src/api/business/erp/customer-price.ts`（新） | 8 个维护接口 + 取价接口的类型与函数 |
| 前端 · 页面 | `frontend/src/views/business/erp/customer-price/{index.vue, customer-price-query.ts}`（新） | 双 Tab：客户协议价 / 阶梯价·促销价 |
| 前端 · 订单增强 | `frontend/src/views/business/erp/sales-order/{index.vue, sales-order-query.ts}`（改） | 自动带价 + 来源标签 + 手动价 + 重取价 |
| 前端 · 共享件 | `frontend/src/views/business/erp/shared/price-source.ts`（新） | 取价来源的唯一一份译名 / 配色 |
| 前端 · 路由 | `frontend/src/router/index.ts`（改） | `business/erp/customer-price` |
| 验证脚本 | `scripts/verify/so34_instance.sh`、`so34_price_admin_e2e.py`、`so34_curl_evidence.sh`（新） | 隔离实例 + 46 条断言 + curl 原文 |

### 后端为什么要写 CRUD（本卡不是纯前端）

SO-33 只交了 `GET /price/resolve`（读口径），价格表的 **CRUD 一直没有接口**（SO-32 残留）。
「客户协议价列表 + 新建/编辑 + 阶梯价维护」没有写接口就是死页面，所以本卡补齐了：
`/price/agreed` 与 `/price/tier` 各 list/create/update/delete，共 8 个。

## 2. 关键口径（会算错钱的地方）

1. **档位区间左闭右开**：`min_qty <= qty < max_qty`，`max_qty` 空 = 无上限（与取价侧 `tier_covers` 同口径）。
2. **有效期两端都含**：`effective_to = effective_from` 是「只当天有效」，`to < from` 入口 400。
3. **价格必须 > 0**：0 元是**单据行**的能力，写进价格表会让取价静默命中 0 元价。
4. **手动价不被自动覆盖**：销售改过价后（`price_manual = true`），换客户 / 改数量只更新来源标签，
   价格保持人工值；要看规则价点「重取价」（先清标记再覆盖）。
5. **换物料先清价**：取价失败时旧物料的价格绝不会留在新物料行上（最典型的卖错价路径）。
6. **乱序响应丢弃**：每行一个请求序号，慢响应回来若序号已过期直接丢弃（`qty` 连敲时的经典 bug）。
7. **`NONE` 是 200**：无价是合法业务结果 → 页面提示「请手填」，不清空用户已填的数、不弹错误。

## 3. 验证结果（全部真实执行）

| 项 | 结果 | 证据 |
|----|------|------|
| `cargo test --lib` | **2946 passed / 0 failed**（本卡 +24：model 10 / handler 14） | `cargo_test_lib.txt` |
| `cargo clippy --lib` | 本卡两文件 **0 warning**（receipt.rs 的 13 条是工作区里他人的未提交改动，未触碰） | — |
| `pnpm typecheck` | 通过 | `frontend_build.txt` |
| `pnpm build` | 通过，产物含 `customer-price-*.js` | `frontend_build.txt` |
| V0308 迁移 | 启动「应用 1 个迁移」；`308|versioned|executed` | `server_startup_migration.log` |
| 迁移幂等 / 回滚 | 二次 up no-op；down 删 8 行；二次 down no-op；再 up 恢复 8 行；`applied_via` 非法值 0 行 | `migration_idempotency.txt` |
| 菜单 / 授权落库 | 页面 1 + 按钮 7，父级均为「价格管理」；SALES_SUPERVISOR 9 码 / SALES_CLERK 3 码 / FINANCE_MANAGER 1 码 | `db_menu_state.txt` |
| `system::menu::tests::n08` | 104 条菜单路径全部能在前端路由解析 | 终端输出（`cargo test --lib system::menu::tests::n08`） |
| `perm_menu_gate.py` | missing_in_menu = **0**，闸 PASS | 终端输出 |
| HTTP e2e（真实 token） | **46 / 46 ALL_PASS** | `e2e_price_admin.txt` |
| curl 原文 | 登录 → 建价 → 分页 → 建档位 → 409 → 取价 2 例 → 开单 → 删除 → 401 | `curl_transcript.txt` |
| UI（headless Chrome，真后端 :18241） | 见 §4 | `ui_tier_tab.png` 等 |

## 4. 界面手工验证（真实浏览器，非模拟数据）

环境：`vite :5199`（`BACKEND_ORIGIN=http://127.0.0.1:18241`）+ `cloudpivot_so34` 真库，登录 `admin`。

| 步骤 | 观察到的实际结果 |
|------|------------------|
| 打开 `/business/erp/customer-price` | 双 Tab 渲染，协议价列表 7 条真实数据，列含客户 / 物料 / 单价 / 起订量 / 有效期 / 状态 |
| 点「新建协议价」 | 弹窗打开；选 客户 CUS-0001 + 物料 SO33-M4 + 单价 15.5500 → 创建成功，列表新增该行（共 8 条） |
| 该行点「删除」 | `DialogPlugin` 确认框（文案含软删说明）→ 确认后列表回到 7 条 |
| 切「阶梯价 / 促销价」Tab | 3 条真实档位：[0,100) 促销 6.50 / [0,100) 阶梯 7.00 / [0,+∞) 通用 8.00 |
| 销售订单 → 新建 → 选客户 C2 + 物料 M4（qty=1） | 取价来源 = **未找到**，toast「第 1 行未找到协议价 / 阶梯价 / 标准价，请手填含税单价」，单价保持空 |
| 数量改 60 | 单价 **自动带出 9.9999**，来源标签 **协议价**，金额 599.99、税额 78.00、价税合计 677.99 |
| 单价手改为 8.8888 | 标签变为 **手动价**，金额 533.33 / 税额 69.33 / 合计 602.66 |
| 点「重取价」 | 单价回到 9.9999，标签回到 **协议价** |
| 点「创建」 | 弹窗关闭无错误，列表出现 `SO-20261107-000008`（客户 B / 677.99 / 草稿） |

> 说明：`SO-34` 的订单带价是**前端调 `/price/resolve` 后把价填进单据行**，来源不进单据
> （后端 `SalesOrderItemReq` 只收 `qty/price/tax_rate`）。要追溯「这一行为什么这个价」，
> 用 `source_id` 回查那一行价格配置。

## 5. 复现

```bash
# 库：cloudpivot_so34 = cloudpivot_so33 的模板克隆（已过 V0307，含 SO-33 夹具）
runuser -u postgres -- dropdb --if-exists cloudpivot_so34
runuser -u postgres -- createdb -O alun -T cloudpivot_so33 cloudpivot_so34

bash scripts/verify/so34_instance.sh up          # :18241，启动时应用 V0308
python3 scripts/verify/so34_price_admin_e2e.py   # 46 断言
bash scripts/verify/so34_curl_evidence.sh        # curl 原文

cd frontend && pnpm typecheck
NODE_OPTIONS=--max-old-space-size=6144 pnpm build
```

## 6. 残留 / 已知

- **构建需要更大 node heap**：本机默认 heap（~1.5 GB）下 `pnpm build` 会 `JavaScript heap out of memory`；
  `NODE_OPTIONS=--max-old-space-size=6144` 通过。`scripts/build.sh` 未设该变量，是否写死属构建配置决策，本卡未改。
- **物料标准售价（取价第 ④ 级）仍无维护入口**：`biz_material.standard_price` 只能在物料页 / SQL 改，
  本卡只做协议价与档位两张表。
- V0307 的隐藏节点「价格查询」（`business:erp:price:resolve`）保持隐藏，本卡另建了可见的
  「价格管理」页（`cust-price:list`）—— 两个码职责不同（取价只读 vs 配置维护），未合并。
- 隔离库 `cloudpivot_so34` 保留；实例与 vite dev server 已在验证后停掉。
