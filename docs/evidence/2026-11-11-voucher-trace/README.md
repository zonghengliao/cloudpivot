# VTRACE-6 凭证溯源端到端验收证据（2026-11-11）

一句话结论：**6 类凭证来源全部验过 —— 单据能过账、凭证能按来源反查、审批能反查、借贷平衡、前端能解析出跳转路径；一条命令 73 条脚本断言 + 12 条跳转断言全部通过（`FAIL=0`），失败可定位到 `STEP n`。**

```bash
# 一条命令跑完（默认 6 类全跑）
VTRACE_BASE=http://127.0.0.1:18127/api VTRACE_DB_NAME=cloudpivot_vtrace6b \
  bash scripts/verify/vtrace_e2e.sh          # 退出码 0 = 全过
# 汇总行：PASS=73 FAIL=0（另有跳转校验 12 条由 tsx 打印，合计 85 条）
```

> 本次取证环境：**全新空库** `cloudpivot_vtrace6b`（迁移 + 种子自动跑完，不依赖 demo_seed），
> 后端 `127.0.0.1:18127`，前端 Vite `127.0.0.1:5517`（`BACKEND_ORIGIN=http://127.0.0.1:18127`）。
> 同一份库上连跑两遍：第二遍 `PASS=73 FAIL=0`、`残留核对 = 0` —— 脚本可重跑（幂等）。

---

## 一、卡面 6 类 → 真实 `biz_voucher.source_type`

卡面（VTRACE-6 body）写的 `PURCHASE / RETURN / EXPENSE` **不是** `biz_voucher.source_type` 的取值，
它们是 `biz_material_lot.source_type`、成本归集来源等别的轴上的词（对照表见
`frontend/src/views/business/erp/shared/source-route.ts` 文件头）。本卡按「业务单据 → 凭证来源」的
确定映射取最贴近的真实值，**没有跳类型、也没有把卡面词硬塞进映射表**：

| 卡面词 | 真实 source_type | 业务单据 / 生成路径 | 本卡怎么造的 | 跳转解析结果 |
|---|---|---|---|---|
| PURCHASE | `STOCK_IN` | 采购订单 → 审批 → 收货单过账（`voucher.rs:1553`） | **真实 API 全链路**：建 PO → 提交 → 审批 → 从 PO 生成收货单 → 过账（PENDING_POST→审批→POSTED） | `/business/erp/stock-in?in_id=<收货单 id>`（detail） |
| RETURN | `STOCK_OUT` | 出库单过账（退货出库 / 领用共用同一来源值，`voucher.rs:2696`） | **真实 API**：建出库单（ISSUE）→ 过账 | `/business/erp/stock-out?out_no=<出库单号>`（filter） |
| EXPENSE | `EXPENSE_REPORT` | 报销单审批通过自动入账（`expense_bridge.rs:433`） | **真实 API**：建报销单 → 上传发票 → 提交 → 审批通过 → 自动生成凭证 | `/business/erp/expense`（none，只落列表） |
| DEPRECIATION | `DEPRECIATION` | 折旧批次计提（`depreciation_model.rs:456`） | **真实 API**：建资产卡（顺带生成 FIXED_ASSET_ACQ）→ `POST /depreciation/run` 计提次月 | `/business/erp/depreciation`（none，只落列表） |
| COST_PERIOD | `COST_PERIOD` | 成本核算结算（`cost_settle_voucher.rs:89`） | **[FIXTURE]** 批次走 `POST /cost-period`；结算凭证按 `post_all` 落库形态直接插入（见「四、限制」） | **null（不给假链接）**：`deepLink=page-missing` |
| MANUAL | `MANUAL` | 手工凭证（`voucher_write.rs`） | **真实 API**：建凭证 → 审批人审核（制单人不能自审）→ 记账 | **null**：手工凭证无来源单据 |

---

## 二、每类断言了什么（4 条硬指标）

| # | 指标 | 对应断言文本 |
|---|---|---|
| ① | 单据真的过账了 | `⑥ 收货单已过账 POSTED` / `③ 出库单已过账 POSTED` / `报销单变 APPROVED` / `②b 批次状态 POSTED` / `③b 凭证已记账 POSTED` |
| ② | `GET /business/erp/voucher/by-source` 能按 (source_type, source_id) 反查到凭证，且 `source_no`/`source_id` 对得上 | `【溯源①】…` 4 行/类 |
| ③ | `GET /business/flow/instance/by-biz` 能反查到审批单 | `【溯源②】…`（PURCHASE / EXPENSE；STOCK_OUT 免审直过、DEPRECIATION 计提不走审批、MANUAL 无审批 —— 见「四」） |
| ④ | 凭证详情 `balanced=true`，且分录借方合计 == 单头借方 | `【溯源③】…` |
| ⑤ | 真实 (type, id, no) 喂给 `resolveSourceRoute()`：可跳的给出 path+query 且目标页已注册；不可跳的给原因不给假链接 | `STEP 7`（`71-jump-paths.log`） |

逐条清单见 **`72-assertions.txt`**（84 行 `[PASS]`、0 行 `[FAIL]`；= 脚本 73 条 − 1 条 STEP 7 汇总 + 跳转 12 条）；完整原始日志见 **`00-run.log`**。

---

## 三、文件索引

| 文件 | 内容 |
|---|---|
| `00-run.log` | 一条命令的完整输出（含每一步的 PASS/FAIL 与最终汇总） |
| `72-assertions.txt` | 断言清单（从日志抽取，按 STEP 分组） |
| `10-…19-*.json` | PURCHASE 链：建单 / 提交 / 审批 / 收货单 / 过账 / 详情 / **by-source** / **flow by-biz** / 凭证详情 |
| `20-…24-*.json` | RETURN（出库单）：建单 / 过账 / **by-source** / 凭证详情 |
| `30-…36-*.json` | EXPENSE：建单 / 发票附件 / 提交 / 审批 / **by-source** / **flow by-biz** / 凭证详情 |
| `40-…43-*.json` | DEPRECIATION：建卡 / 计提 / **by-source** / 凭证详情 |
| `50-…52-*.json` | COST_PERIOD：建批次（真实接口）/ **by-source** / 凭证详情（凭证为夹具） |
| `60-…64-*.json` | MANUAL：建凭证 / 审核 / 记账 / **by-source（空数组）** / 凭证列表兜底 |
| `70-jump-cases.json` `71-jump-paths.log` | 跳转校验的输入（真实 id/单号）与 `resolveSourceRoute()` 逐条结果 |
| `90-voucher-detail-source-link.png` | 凭证详情：来源单据 `IN-20261111-000003` 是可点链接 |
| `91-jump-to-stock-in-detail.png` | 点链接后落到 `/business/erp/stock-in` 并自动打开收货单详情抽屉 |
| `92-voucher-full-trace.png` | 「全链路」标签页：单据 → 审批（含审批人与意见）→ 本凭证 → 分录（借贷平衡） |
| `93-manual-voucher-plaintext.png` | 手工凭证：来源单据是纯文本（`手工凭证 100`），没有假链接 |

`01-login.json` 里的 `access_token` / `refresh_token` 已脱敏为 `<redacted>`，不含任何可用凭据。

---

## 四、限制与已知缺口（不粉饰）

1. **COST_PERIOD 的凭证是夹具**（日志里标 `[FIXTURE]`）。真实结算要求先有 `calc` 计算单，
   而计算单需要 BOM / 工艺路线 / 人工与制造费用归集整套夹具（数小时级），超出本卡 ≤1.5h 的范围。
   本卡用夹具只验「按来源反查 + 跳转映射」，**真实结算生成**由 COST-B3-6 的真库测试覆盖（`cost_settle_run`）。
   批次单据本身是真接口建的（`POST /business/erp/cost-period`）。
2. **COST_PERIOD 没有可跳页面**：`source-route.ts` 里该行 `deepLink='page-missing'`（`views/business/erp/cost-period/`
   只有 `steps/`、`router/index.ts` 未注册），`resolveSourceRoute()` 返回 `null` —— 脚本按「不可跳 + 期望着 null」断言，
   **这是预期**，不是回归失败。页面落地后把该行 `deepLink` 改 `detail` 即自动翻转。
3. `EXPENSE_REPORT` / `DEPRECIATION` 的 `deepLink='none'`：目标页还没读 `report_id` / `batch_id`，
   所以只落列表页、不塞假参数。要「跳进详情」得改那两个列表页读 query（不属本卡）。
4. STOCK_OUT / DEPRECIATION 没有审批段可比（出库过账免审直过、折旧计提不走审批流），
   因此这两类只断言 ①②④，不硬造 `【溯源②】`。

## 五、本次发现的三个真问题（已记入卡内汇报）

1. **折旧凭证的 `source_no` 是期间码，不是批次号**
   （`PZ-20261111-000017`：`source_no='2026-12'`、`source_doc_no='2026-12'`，而 `source_id` 才是 `biz_depreciation_batch.id`）。
   影响：凭证详情「来源单据」列显示 `2026-12`（标签却是「折旧批次」），人会以为单号是期间。
   跳转不受影响（映射行 `by='id'`）。未在本卡改业务代码 —— 改落库值会动历史凭证，建议单开卡（`depreciation_model.rs` + 已落库数据回填）。
2. **空库首启的 admin 所有业务接口 403**（`session_guard`：`首次登录必须先修改初始密码`）。
   脚本已内置「先改密再登」并支持重跑命中派生口令，否则新环境上 e2e 会全红且看着像权限码缺失。
3. **MANUAL 凭证 `source_id` 恒为空**，所以「按来源反查」对手工凭证永远是 `[]`（设计事实）。
   溯源兜底路径是凭证列表（脚本里 `64-voucher-page-manual.json` 断言了这一点）。

## 六、复现步骤

```bash
# 1) 全新隔离库 + 后端（迁移与种子自动跑）
su - postgres -c "psql -c 'CREATE DATABASE cloudpivot_vtrace6 OWNER alun'"
cd /opt/cloudpivot/backend
env NIK_PROFILE=test NIK_ENV=test NIK_SERVER_LISTEN=127.0.0.1:18127 \
    NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 \
    NIK_DATABASE_NAME=cloudpivot_vtrace6 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
    NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
    CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
    X_CONFIG_SECRET_KEY=$(openssl rand -hex 16) \
    NIK_ADMIN_PASSWORD='Admin@123456' \
    ./target/debug/cloudpivot &

# 2) 跑验收
cd /opt/cloudpivot
VTRACE_BASE=http://127.0.0.1:18127/api VTRACE_DB_NAME=cloudpivot_vtrace6 \
  bash scripts/verify/vtrace_e2e.sh        # --keep 保留夹具，--only <类> 单跑一类

# 3) 前端截图（可选）
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18127 pnpm exec vite --port 5517 --host 127.0.0.1
# 浏览器打开 http://127.0.0.1:5517/business/erp/voucher，按凭证号筛 PZ-…-000013 → 查看 → 详情 / 全链路
```

脚本自建/自清：主数据夹具（`E6VT-*` 单位 / 供应商 / 仓库 / 物料）、单据（采购单 / 收货单 / 出库单 / 报销单 /
资产卡 / 折旧批次 / 成本批次 / 手工凭证）、凭证与分录、审批实例与待办、发票附件绑定行，
以及临时审批人账号与 `sys_dept.leader_id`（退出时 `trap` 恢复原值）。跑完 `残留核对 = 0`。
