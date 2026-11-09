# E2E05-FIX-2 证据：资产启用不再被附件卡死（方案 B：提示 + 允许 + 审计）

卡：`t_b1bf7566`（父卡 `t_abb4bb0b` E2E05-FIX-1）
代码提交：`635b07fe`（主实现）+ `a03226d3`（规则文档 + 验收脚本支持覆盖库名）
环境：145 测试机 · 库 `cloudpivot_dev` · 隔离后端实例 `127.0.0.1:8037`（干净克隆 `/opt/cp-e2e05fix2` @ `a03226d3`）

---

## 1. 问题（E2E-05 演练踩到的死路）

| 路径 | 改造前 |
|------|--------|
| 建卡（直接 `IN_USE`） | 不查附件（业务需要：发票可能未到） |
| 停用 → 再启用（`IDLE → IN_USE`） | **400「固定资产必须上传发票或验收单(pdf/jpg/png)：至少要 1 个」** |

后果：「发票未到先建卡」的既有资产**停用一次就永远启用不回来** —— 界面上「确认启用」还置灰，
没有待办、没有手工接口，只能改库。问题不在按钮文案，在**规则只在状态边生效**。

## 2. 口径选择：方案 B（卡面推荐）

- **方案 A**（建卡也强制附件）一致性最好，但「发票未到先建卡」的真实业务做不了；
- **方案 B**（采纳）：**建卡口径不变**；启用降级为「提示 + 允许 + 审计」。
  理由：资产已经存在（停用只是既有状态），不能用附件把它锁死；但放行必须留痕。

| 场景 | 改造后行为 |
|------|-----------|
| 启用 + 附件齐备 | 与改造前**完全一致**：`warning = None`、无审计 |
| 启用 + 缺附件 | **200 照常启用**；响应带 `warning`；直写一条 `operation_logs`（`ERP_ASSET_ENABLE_NO_ATTACH`）；`tracing::warn!` |
| 启用 + 规则求值失败（读库抖动） | 同样放行，但 warning/审计都写明「**未校验**」，绝不静默当成「附件齐备」 |
| 其余单据（发票 / 收款 / 付款 / 报销 / 盘点 / 退货） | **不变**，仍是 `ensure_attachments` 硬闸 |

审计写入时机：`tx.commit()` **之后**（状态已改成功），写失败只 `tracing::warn!` —— 不让一次审计写库失败把已经成功的启用变成 500。

## 3. 改动清单

| 文件 | 改动 |
|------|------|
| `backend/src/business/erp/asset_handler.rs` | 启用分支 `ensure_attachments(..)?` → `check_attachments(..)` + 放行 + warning + 审计；新增 `OP_ENABLE_WITHOUT_ATTACH` / `EnableWithoutAttachmentLog` / `write_enable_without_attachment_log` / `activate_attachment_warning`（纯函数）；`#[cfg(test)] mod enable_warning_tests`（3 条） |
| `backend/src/business/erp/asset_dto.rs` | `AssetDetailRes.warning: Option<String>`（`skip_serializing_if`，其它接口不变） |
| `backend/src/business/erp/asset_linkage_tests.rs` | 新增 `st02_enable_without_attachment_is_allowed_and_audited`（真库）；清场 SQL 修 `operation_logs.target_id` 是 varchar 却用 `::uuid` 比的问题 |
| `backend/src/business/attachment/rule.rs` | 文件头「触发动作对应表」注明这是**唯一例外**，防后来人照抄成「都只警告」 |
| `frontend/src/views/business/erp/asset/index.vue` | 「确认启用」不再置灰；缺附件 → 弹窗黄字点名 + 二次确认框；响应 `warning` → warning toast；建卡提示/附件页签文案改成「缺了仍可启用，但会记审计」 |
| `frontend/src/api/business/erp/asset.ts` | `AssetDetail.warning?: string` |
| `scripts/verify/e2e05fix2_enable_no_attach_e2e.sh` | 145 真 token curl 验收（覆盖卡面 ①~⑤ + `attachment/check` 预检），`DB_NAME` 可覆盖 |

## 4. 单测 / 构建（99 工作仓）

```
cargo test --lib business::erp::asset_handler::enable_warning_tests   → 3 passed / 0 failed
cargo test --lib business::erp::asset_linkage_tests                   → 12 passed / 0 failed
cargo test --lib business::erp::asset                                 → 65 passed / 0 failed
（真库：ERP_DB_URL=…/cloudpivot_e2e05fix2，295 条迁移，--test-threads=1）
cargo check --lib / cargo check --tests                               → 0 error 0 warning
rustfmt +1.94.1 --edition 2024 --check（本卡 3 个 .rs）                 → 无 diff
pnpm typecheck                                                        → exit 0
pnpm build                                                            → 成功（dist 产出）
```

`enable_warning_tests` 钉住三件事：无缺口 → `None`（正常启用不该弹框）；缺一条 → 点名需求名 + 带审计码；
缺多条 → 顿号连接且审计码只出现一次。

## 5. 145 真机验收（`bash scripts/verify/e2e05fix2_enable_no_attach_e2e.sh`）

`145_run.out`（逐条断言）+ `asserts.log`：**PASS=50 FAIL=0**。要点：

| 卡面验收 | 实测 |
|---------|------|
| ① 建卡不传附件 → 正常建成 | 200/0，状态 `IN_USE`，附件 0，无审计行 |
| ③ 前端预检数据源 | `GET /attachment/check?at=ACTIVATE` → `can_proceed=false` + 缺口 `hint`；补传后 `true` |
| ② 停用 → 无附件启用 | **200**（改造前 400）；`warning` 点名「发票或验收单」并带 `ERP_ASSET_ENABLE_NO_ATTACH`；状态真回 `IN_USE`；`operation_logs` **恰好 1 行**（asset_no / reason / `attachments_satisfied=false` 与响应同源）；留痕有 `ENABLE` 行 |
| ④ 有附件启用 | 200，**无** warning，审计仍 1 行（不新增） |
| ⑤ 建卡带附件 | 200，附件落库，`can_proceed=true` |

夹具：`E2E05FIX2-11092108 无附件资产`（`FA-20261109-000035`）、`… 建卡带附件`；分类 `E2E5F2_60`；
期间 `2032-01`（本卡独占）。脚本按 `RUN_TAG` 每次造新夹具 ⇒ 可反复跑。

## 6. 前端交互实测（145 nginx 托管 `frontend/dist`，admin 真 token 注入 localStorage）

`ui-confirm-dialog.png` + 浏览器 DOM 读数：

1. 「启用」弹窗（`asset-status-dialog`）**按钮不再置灰**（`disabled=false`）；
2. 弹窗黄字（`asset-enable-gate-hint`，文案来自后端 `hint`）：
   `缺少发票或验收单（需 pdf/jpg/png 格式）：本次启用仍会成功，但会记一条审计（操作日志 ERP_ASSET_ENABLE_NO_ATTACH），建议尽快补传。`
3. 点「确认启用」→ 二次确认框：
   `未上传发票 / 验收单 … 继续启用「FA-20261109-000037 …」不会失败，但系统会记一条「无附件启用」审计（操作日志 ERP_ASSET_ENABLE_NO_ATTACH）… 确认继续？`（按钮：先去上传 / 无附件启用）
4. 点「无附件启用」→ 后端 200，warning toast：
   `本次启用未上传发票或验收单，已记审计（操作日志：ERP_ASSET_ENABLE_NO_ATTACH）；建议尽快在「资产详情 → 附件」补传`
   行状态变「在役」。

`ui_db_check.log`（同一张卡的库侧结果）：

```
ERP_ASSET_ENABLE_NO_ATTACH|admin|biz_fixed_asset|FA-20261109-000037|E2E05-FIX-2 UI 验收：发票未到，先恢复使用|false|200
FA-20261109-000037|IN_USE|0.0000        ← 卡片已在役、未计提
附件数 = 0                                ← 确实是在无附件下完成的
变动留痕：IDLE|IN_USE（停用） / ENABLE|IDLE|IN_USE（无附件启用）
```

## 7. 边界与注意事项

- **审计是旁路**：状态先提交成功，再写审计；审计写失败只 warn（否则用户看到 500 但状态已变）。
- **规则求值失败 = 未校验放行**，不是「附件齐备」：warning 与审计都写明原因，避免一次 DB 抖动
  把「必传」悄悄变成「不用传」而不留痕。
- **只降级了固定资产启用这一处**：`ensure_attachments` 仍是其余 7 类单据的硬闸（`rule.rs` 文件头已注明）。
- 审计操作码 `ERP_ASSET_ENABLE_NO_ATTACH` 可作操作日志页的筛选条件（「哪些资产是无票启用的」）。
- 前端仍保留「去上传」链接：规则没取消，只是不拦；建议补传。

## 8. 复跑方式

```bash
# 145（测试机）
git clone /opt/git/cloudpivot.git /opt/cp-e2e05fix2 && cd /opt/cp-e2e05fix2 && git checkout a03226d3
cd backend && cp config/config-dev.example.toml config/config-dev.toml && cargo build --bin cloudpivot
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
NIK_SERVER_LISTEN=127.0.0.1:8037 NIK_CACHE_TYPE=redis NIK_REDIS_URL=redis://127.0.0.1:6379/3 \
  ./target/debug/cloudpivot &          # 注意：8033 常被别的 worker 的实例占着
cd /opt/cloudpivot && WORK=/tmp/e2e05fix2_145 BASE=http://127.0.0.1:8037/api \
  bash scripts/verify/e2e05fix2_enable_no_attach_e2e.sh
```

> 145 上 `/etc/cloudpivot/cloudpivot-dev.env` 的 `NIK_DATABASE_NAME=cloudpivot_dev` 必须存在；
> 若换库，脚本支持 `DB_NAME=<库名>` 覆盖（实例与脚本要指同一个库）。
> profile=dev 有启动守卫：`[cache] type` 必须是 `redis`（clone 里 `config/config-dev.toml`
> 由 `config-dev.example.toml` 复制，或 `NIK_CACHE_TYPE=redis`）。
