# P2-01 询比价闭环 · 验收证据（2026-11-08）

> 卡面：t_f5ccdb6c —— 询比价：多家报价 → 比价 → 定标 → 转采购订单
> 结论：**验收 1–6 全部 PASS**；全链 API 实测 **93 条断言 0 失败**；DB 侧判据 **7/7 PASS**。

## 0. 本次实测的机器与隔离环境（不碰 144 / 不碰 8033）

| 项 | 值 |
|----|----|
| 主机 | `172.18.10.99`（工作仓 `/opt/cloudpivot`） |
| 后端进程 | `backend/target/debug/cloudpivot`，`NIK_SERVER_LISTEN=127.0.0.1:18102` |
| 数据库 | **全新空库** `cloudpivot_pi01r5`（owner `alun`，起服时首启迁移 **262** 条，含 V0321） |
| 账号 | `admin / Admin@123456`（首登强制改密已在脚本里走完） |
| 起停脚本 | `~/.hermes/profiles/backend-developer/cache/scratch/pi01_instance.sh`（`PI01_DB=cloudpivot_pi01r5 PI01_PORT=18102 bash pi01_instance.sh start`） |

两次启动的迁移日志：首启应用 262 条；**重启后「应用 0 个迁移」**（迁移幂等，V0321 自检块每次都跑通）。

## 1. 文件清单（交付物）

| 层 | 文件 |
|----|------|
| 迁移 | `backend/migrations/business/V0321__price_inquiry.up.sql` / `.down.sql` |
| 后端 | `backend/src/business/erp/price_inquiry_dto.rs` / `_model.rs` / `_handler.rs`，`erp/mod.rs`（注册 + 模块表） |
| 错误码 | `backend/src/shared/error_code.rs`：`ERP_PRICE_INQUIRY_AWARD_GUARD = 66047`（定标闸专用 409 码） |
| 前端 | `frontend/src/api/business/erp/price-inquiry.ts`、`frontend/src/views/business/erp/price-inquiry/{index.vue,price-inquiry-logic.ts}`、`frontend/src/router/index.ts` |
| 菜单/权限 | V0321 内：页面节点 `…3321` + 4 个按钮 `…3322–3325` + 5 个权限码 + `PURCHASE_MANAGER` 全授 / `WAREHOUSE_MANAGER` 只读 |

## 2. 接口（真实前缀 `/api/business/erp/...`）

| 方法 | 路径 | 权限码 |
|------|------|--------|
| POST | `/api/business/erp/price-inquiry` | `business:erp:price-inquiry:create` |
| GET | `/api/business/erp/price-inquiry/page` | `…:list` |
| GET | `/api/business/erp/price-inquiry/{id}` | `…:list`（详情含**比价表** compare） |
| POST | `/api/business/erp/price-inquiry/{id}/quote` | `…:quote` |
| POST | `/api/business/erp/price-inquiry/{id}/award` | `…:award` |
| POST | `/api/business/erp/price-inquiry/{id}/to-po` | `…:to-po` |

与卡面差异（卡面写 `/erp/price-inquiry`）：前缀由 `[router].prefix=/api` 注入、业务容器段 `business`，与采购订单等既有模块一致。

## 3. 验收标准逐条对照

| # | 卡面验收 | 证据（见 `01_api_steps.txt` / `03_db_assert_output.txt`） |
|---|----------|----------------------------------------------------------|
| 1 | 建询价单（3 个物料） | `PIQ-20261108-000001`，初始 `DRAFT`，明细 3 行，单位由物料带出，采购员=当前登录人，`item_count=3` |
| 2 | 3 家供应商分别报价 | 3 家 × 3 行 = **9 条**报价，状态自动推进 `DRAFT→QUOTING`，含税单价服务端算 `100×1.13=113.000000`，同供应商重复报价 = 改价（200 而非新行） |
| 3 | 比价表横向对比 | 每行 3 家：价格 / 交期 / 最低价标记均可横向比（行 1 最低价唯一 = 甲 100.000000；交期 甲30/乙15/丙7） |
| 4 | 定标 A 供应商 | `award_mode=MULTI`，`awarded_at` 落库，中标标记**只落在 A 的 3 条报价**上（DB 判据 B：9/3/3、中标只落一家=1） |
| 5 | 转采购订单，单价 = A 报价 | `PO-20261108-000001` 3 行，逐行 `price=中标报价`、`qty=询价数量`、`tax_rate=13%`（C 判据 3/3/3）；不含税 1750、价税合计 1977.5；纯 SQL 重算 `Σ(中标单价×数量)` = 采购单不含税额（M 段 `match=t`） |
| 6 | 全链 API 实测 + DB 断言 | API **93 断言 0 失败**；DB 汇总判据 **7/7 PASS**（A/B/B/G/C/E/J） |

### DB 级兜底（`03_db_assert_output.txt` 的 O / P 段）

- `uk_biz_price_quote_awarded`（`CREATE UNIQUE INDEX … (item_id) WHERE is_awarded`）存在；
- **负向注入**：同一明细再插第二个 `is_awarded=TRUE` 的报价 → 被 `unique_violation` 拒绝（`PASS: 同一明细第二家中标被 uk_biz_price_quote_awarded 拒绝`），事务 `ROLLBACK`；
- 收尾复查 `SELFTEST-AWARDED-ONLY` 残留 = **0**（负向注入不污染证据库）。

### 反证与边界（都在 `01_api_steps.txt` 里）

- 重复定标 → **409 / 66047**；重复转单 → **409 / 10003**（已 CONVERTED）
- 单源：只有 1 家报价且没填理由 → **409 / 66047**，文案点名 `single_source_reason`；理由 < 10 字同样 409；填了 ≥10 字 → 200，`award_mode=SINGLE_SOURCE`（DB 侧 CHECK `ck_biz_piq_award_reason` 兜底）
- 反证：中标供应商**没报过价** → 409 / 66047，文案说明缺报价
- 一单拆多单：物料 1 → A、物料 2 → B，转单产出 **2 张采购单**，分别挂 A/B，金额各自 135.6 / 406.8
- 分页过滤：状态过滤只返回 CONVERTED；关键字命中单号；非法状态 → 400

## 4. 质量门（本次实跑）

| 门 | 命令 | 结果 |
|----|------|------|
| 后端单测（C7） | `cargo test --lib price_inquiry` | **20 passed / 0 failed**（dto 17 + model 3，见 `raw/10_backend_unittest.txt`） |
| 编译 | `cargo build` | `BUILD_EXIT=0` |
| Clippy（本卡文件零告警） | `cargo clippy --lib --tests` | 退出 0；7 条告警全在**既有模块**（asset_linkage / fx_revaluation / opening / sales_order_model / voucher / year_end_linkage_tests），`price_inquiry_*` **0 条**（`raw/11_clippy.txt`） |
| 格式 | `rustfmt +1.94.1 --edition 2024 --check`（本卡 3 文件 + error_code.rs） | 0 diff |
| 前端类型 | `pnpm typecheck` | `TC_EXIT=0` |
| 前端构建 | `pnpm build`（`NODE_OPTIONS=--max-old-space-size=3072`） | `FE_BUILD_EXIT=0` |
| 死权限码常驻闸 | `perm_menu_gate.py`（连 r5 库） | **PASS**：`missing_in_menu = 0`（357 个注解码全部有菜单承载）；`unused_in_code = 12` 全是既有行级/纯前端码，本卡 5 个码一个都不在列表里（`raw/14_perm_menu_gate.txt`） |

## 5. 复跑清单

```bash
# 1) 起隔离实例（全新库；已在 99 建好 cloudpivot_pi01r5，可换库名复跑）
PI01_DB=cloudpivot_pi01r5 PI01_PORT=18102 \
  bash ~/.hermes/profiles/backend-developer/cache/scratch/pi01_instance.sh start

# 2) API 全链（自己建 3 家供应商与 4 张询价单，重复跑用新库）
P201_BASE=http://127.0.0.1:18102 \
P201_OUT=/opt/cloudpivot/docs/evidence/20261108-p201-price-inquiry \
  python3 p201_price_inquiry_e2e.py

# 3) DB 侧断言
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_pi01r5 -f 02_db_assert.sql

# 4) 后端单测
cd /opt/cloudpivot/backend && cargo test --lib price_inquiry
```

> 脚本自带限流自愈（登录 429 退避重试）与幂等（供应商按 code 复用；同一库复跑会追加新的询价单，DB 断言里硬编码的单号只在**全新库首跑**成立）。

## 6. 已知限制（如实记录，未在本卡修）

1. **转单不是跨事务原子**：`to-po` 锁住询价单头（`SELECT … FOR UPDATE`），但每张采购订单由 `purchase_handler::create_purchase_order` 各自独立事务生成。若多供应商拆单中途某张失败，已生成的采购单**不会**回滚，询价单仍是 `AWARDED`；重试会对已成功的供应商**再生成一张**。单供应商场景（本卡验收路径）不受影响。修法需要采购模块暴露「在外部事务内建单」的入口，属跨模块改造，未擅自改采购底座。
2. **定标按明细行唯一已升级为库级约束**：`uk_biz_price_quote_awarded`（部分唯一索引）兜住「同一明细两家中标」；handler 的「先清后置位」与大事务顺序与之相容（清/置位同一事务，逐条置位时同一 `item_id` 只会被置一次，DTO 的 `award_rejects_duplicate_line` 已挡重复）。负向注入见 DB 断言 O 段。
3. 卡面要求的「前端页面」验证口径是 **typecheck + build 全绿 + 路由/菜单节点一致**；未做浏览器端点击级 E2E（本仓无前端 E2E 设施）。

## 7. 原始日志索引（`raw/`）

| 文件 | 内容 |
|------|------|
| `raw/10_backend_unittest.txt` | `cargo test --lib price_inquiry` → 20 passed / 0 failed |
| `raw/11_clippy.txt` | `cargo clippy --lib --tests` 全量告警（7 条，均非本卡文件） |
| `raw/12_fe_typecheck.txt` | `pnpm typecheck` → `TC_EXIT=0` |
| `raw/13_fe_build.txt` | `pnpm build` → `FE_BUILD_EXIT=0` |
| `raw/20_e2e_console.txt` | 本轮 API 全链控制台输出（93 通过 / 0 失败） |
| `raw/14_perm_menu_gate.txt` | 死权限码闸（连 r5 库）：`missing_in_menu=0` → PASS |
| `00_api_responses.json` | 每一步的真实响应体（含 409 反证） |
| `01_api_steps.txt` | 逐条断言清单（人读） |
| `03_db_assert_output.txt` | DB 侧全部判据（A–P 段） |
