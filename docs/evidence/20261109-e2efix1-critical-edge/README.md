# E2E-FIX-1 证据：主链路状态边保护 + 禁用告警（卡 t_c99b63cd）

## 一句话

2026-11-09 E2E-02 演练把 `biz_flow_state_edge` 里的 `STOCK_OUT DRAFT→POSTED` 改成
`enabled = false`（属主不明），发货过账当场 409，**整条销售链断在发货**，而且：
界面上看不出「这条边被关了」、没有任何巡检发现它、报错还把内部出库单号抛给销售用户。
本卡把这三件事一起收口：标「主链路必需边」→ 禁用要三次确认（红字警告 + `confirm_critical`）
→ 巡检报警（只读不自动改回）→ 报错改业务语言（单号只进日志）。

## 文件

| 文件 | 作用 |
| --- | --- |
| `backend/src/business/shared/state_machine.rs` | `CRITICAL_EDGES`（25 条主链路边 + 业务后果 `impact`）、`critical_edge()` 查询、禁用前的 `confirm_critical` 硬闸、`transition()` 的 409 报文改业务语言（内部细节只进 `internal_detail`） |
| `backend/migrations/business/V0354__flow_critical_edge_whitelist.*` | `biz_flow_critical_edge` 巡检白名单表（幂等种入 25 行，与 `CRITICAL_EDGES` 逐行一致；真库单测比对防漂移） |
| `backend/src/business/flow/edge_admin.rs` | `EdgeUpsertReq.confirm_critical` 透传、`EdgeRes.critical / critical_impact` 下发、审批回调 `form_data` 往返 |
| `scripts/verify/flow_critical_edges.sh` | 只读巡检：任何 critical 边被禁用 → rc=1 报警（**不自动改回**，避免推翻人工加严） |
| `scripts/verify/run_all.sh` | 巡检并入 ⑤ 不变量阶段（rc=4「白名单表不存在」也算 FAIL，不许假装绿） |
| `frontend/src/views/business/flow/def/StateEdgePanel.vue` | 「主链路」红标 + 禁用时的错误色告警 + 强制勾选「我确认要禁用主链路必需边」 |
| `docs/evidence/20261109-e2efix1-critical-edge/live-145.sh` | 本目录的实测脚本（145 隔离库 + 隔离端口，不碰生产/共享库） |

## 验收对照（全部为真实 HTTP + 真库，非描述）

| 卡面验收 | 实测 | 证据位置 |
| --- | --- | --- |
| ① 禁用 `STOCK_OUT DRAFT→POSTED` 不传 `confirm_critical` → 400 + 警告文案 | ✅ `http=400 code=10001`「这是主链路必需边（STOCK_OUT DRAFT→POSTED），禁用它会让销售出库无法直接过账。确认要禁用请传 confirm_critical=true」，且**没落禁用行** | `live-145.txt` §3 |
| ② 传 `confirm_critical=true` → 成功 | ✅ `http=200 applied=false instance_no=FLW-…`（有已发布流程 → 送审，审批通过才生效） | `live-145.txt` §4 |
| ③ 巡检发现「主链路边被禁用」 | ✅ `flow_critical_edges.sh` rc=1，点名 `STOCK_OUT DRAFT→POSTED 被禁用 → 销售出库无法直接过账` | `live-145.txt` §6 |
| ④ 恢复后巡检通过 | ✅ rc=0「主链路边全部可用（25 条白名单，0 条被禁用）」 | `live-145.txt` §7 |
| ⑤ 用户侧文案不再暴露内部单号 | ✅ 真实出库单 `OUT-20261109-000001` 过账 409 → msg=`销售出库无法直接过账：该操作已被系统配置禁用，请联系系统管理员`（不含单号、不含 DRAFT/POSTED），内部细节只在 `internal_detail`（日志） | `live-145.txt` §5 |
| ⑥ cargo test + push | 见文末「测试」 | — |

## 实测环境

- 145（`ubuntu-svr`）：`git pull` 到 `99da8399` + `cargo build`（debug，2m37s）→
  **全新隔离库 `cp_e2efix1` + 端口 18319**，`schema_migrations` 355/288，白名单 25 行。
  全程不碰 `cloudpivot_dev` / `:8033` / 生产。
- 99（本机）同一脚本先跑过一遍（DB 账号 `alun`），21/21 PASS —— 脚本本身可复现。

## 设计取舍（为什么这么做）

1. **白名单建表而不是给 `biz_flow_state_edge` 加 `is_critical` 列**：
   `biz_flow_state_edge` 是「运维配了什么」（可能根本没有行，走 BASE_EDGES 默认），
   `biz_flow_critical_edge` 是「产品说这条边不能关」。合成一列会漏掉「base 边还没被配置过」
   的那一半；而巡检脚本是 bash + psql，读不到 Rust 常量，必须有库里的副本 —— 两边由
   真库单测 `critical_edges_match_db_table` 逐行比对，改一边不改另一边直接红。
2. **`mandatory` 与 `critical` 分工**：`mandatory = true` 的边在配置层**根本禁不掉**
   （API 400；绕过 API 直写库也会被运行时忽略 + WARN）；`critical = true` 的边**可以禁**，
   但必须显式二次确认。当前"真能被关掉"的主链捷径只有 optional 的
   `STOCK_OUT DRAFT→POSTED`（发货免审直过）与 `STOCK_IN DRAFT→POSTED`（收货免审直过）
   —— 实测时踩到过一次：先关 `DRAFT→PENDING_POST` 想复现 409，结果它是 mandatory，
   运行时直接忽略，PATCH 仍然 200。脚本注释里已把这个坑写清楚，避免下一个人重踩。
3. **巡检只读、不自动改回**：禁用可能是人工加严（例如某月临时关掉免审直过逼着走审批），
   自动改回等于悄悄推翻人的决定。报警 + 人在场判断。
4. **报错文案里不放内部单号**：销售用户看到 `OUT-20261109-000031` 什么也做不了，
   他要的是「找谁」。单号 / 状态名 / 当前可选去向全部进 `internal_detail`（只写日志），
   运维照样能定位。

## 测试

```
# 主链路清单 + 真库白名单比对（含 STOCK_OUT DRAFT→POSTED 回归钉死）
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cp_e2efix1 \
  cargo test --lib -- --test-threads=1 business::shared::state_machine::tests
   → 34 passed / 0 failed
   → [E2E-FIX-1 证据] biz_flow_critical_edge 25 行，与 CRITICAL_EDGES 逐行一致

# 边配置审批链路（新增 critical_edge_disable_needs_confirmation：400 且不留审批单）
ERP_DB_URL=… cargo test --lib -- --test-threads=1 business::flow::edge_admin_tests
   → 25 passed / 0 failed

# 前端
pnpm typecheck                              → 0 错
tsx --test tests/unit/flow-state-edge.test.ts → 14 passed
pnpm build                                  → 成功
```

全量 `cargo test --lib` 的结果见卡内汇报（与本改动无关的既有红见 t_5904f851）。
