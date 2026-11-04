# 【FIN-2-FE】固定资产三页（资产卡片 / 分类 / 折旧计提）· 真机验收证据

卡：`t_bdea38c1` ｜ 设计依据：`docs/architecture/ARCH-6-财务扩展设计.md` §5.10 ｜ 后端：`0eb8b81`（卡 `t_dbc90edc`）
前端代码：`5832df8`（API + 逻辑 + 三页 + 路由 + 24 条单测）+ 本卡收口提交（1280 台账列宽 + 本证据目录）

原始输出在 `raw/`：接口回包 `*.json`、链路日志 `api-chain-log.txt`、UI 断言 `ui-assertions.json` + `ui-log.txt`、
库内快照 `db-snapshot.txt`、门禁 `typecheck.txt` / `unit-fin2-asset.txt` / `unit-all.txt`。

---

## 一、环境（全部隔离，不碰生产）

| 项 | 值 |
|---|---|
| 后端 | `bash scripts/verify/fin2_instance.sh start` → `127.0.0.1:18025`，库 `cloudpivot_fin2asset`（`/health` 200，启动日志「数据库迁移完成：应用 4 个迁移」） |
| 前端 | 干净 worktree（`git worktree add <tree> 119e15e`）+ `BACKEND_ORIGIN=http://127.0.0.1:18025 vite --port 5199`（**源码即产物**，不在 `/opt/cloudpivot/frontend` 里 build，不动生产 dist） |
| 账号 | `admin / Admin@123456`（发起）、`fin2gm / Admin@123456`（总经理审批，角色 `GENERAL_MANAGER`，`assignee_name=FIN2 验收总经理`） |
| 数据 | 4 张卡片（000001–000004）、5 条分类种子、4 个折旧批次（2026-09 … 2026-12 全 `POSTED`）、1 张已过账收货单 `IN-20261105-000002` |

---

## 二、接口全链路（19 步，判据取真回包 + 库内快照）

链路按卡面验收顺序：**建卡 → 看月折旧 → 提交处置 → 换 fin2gm 批 → 作废 → 跑折旧 → 红冲**。

| # | 动作 | 请求 | 实测结果 |
|---|---|---|---|
| ① | 登录 | `POST /api/system/auth/login` ×2 | admin / fin2gm 各拿到 access_token（461 / 487 字符） |
| ② | 分类下拉 | `GET /api/business/erp/asset-category/list` | 5 条：ELEC 电子设备 36 月 / FURNITURE 60 / VEHICLE 48 / MACHINE 120 / BUILDING 240，残值率均 `0.0500` |
| ③ | 台账 | `GET /api/business/erp/asset/page` | `total=2`（本轮建卡前），带 `status_label` 快照、金额字符串 |
| ④ | 建卡 | `POST /api/business/erp/asset`（原值 100000 / 现付 `CASH` / 直购 / 购入 2026-11-10） | `IdRes{no:"FA-20261105-000004", note:"购入凭证 34ff3dd4…"}` —— **同事务生成购入凭证** |
| ⑤ | 详情（口径源） | `GET /api/business/erp/asset/{id}` | 残值 `5000.0000`、可折旧 `95000.0000`、**月折旧 `2638.8889`**、起提 `2026-12-01`、已提 0 月、净值 `100000.0000`、**`months_left=36` / `amount_left=95000.0000`** |
| ⑥ | 提交处置 | `POST /api/business/erp/asset/{id}/dispose` `{SALE, 2026-11-05, 售价 10000, 销项税 1300}` | `code=0`，回 `AssetDetailRes`：状态仍 `IN_USE`，`pending_disposal` 有值（**审批前不动账**） |
| ⑦ | 审批前复核 | `GET /asset/{id}` | `状态=IN_USE(在役) 处置类型=—` ✓ |
| ⑧ | 总经理待办 | `GET /api/business/flow/task/todo`（fin2gm） | 按 `form_data.asset_no` 精确命中 `FA-20261105-000004 出售 处置审批 / 总经理审批（处置不分金额档） / PENDING` |
| ⑨ | 审批通过 | `POST /api/business/flow/task/{tid}/approve` | `instance_status=APPROVED` |
| ⑩ | 审批后 | `GET /asset/{id}` | `DISPOSED(已处置)`、`SALE/2026-11-05`、处置凭证 `792dd676…`、变动留痕 `DISPOSAL` 一条 |
| ⑪ | 作废处置 | `POST /asset/{id}/dispose/cancel {reason}` | `IN_USE`、`disposal_type=null`、`disposal_date=null`（一条 UPDATE 回滚三列，未撞 `ck_biz_fa_disposed`） |
| ⑫ | 计提 | `POST /api/business/erp/depreciation/run {2026-12}` | `outcome=POSTED`、`batch_no=DEP-20261105-000005`、4 台 / 合计 `10555.5556`、凭证 `d0eb41af…`、`message=期间 2026-12 计提完成` |
| ⑬ | 批次分页 | `GET /depreciation/page` | 4 个批次：2026-12 `POSTED` 4 台 10555.5556 / 2026-11 3 台 7916.6667 / 2026-10 1 台 / 2026-09 1 台 |
| ⑭ | 批次明细 | `GET /depreciation/{id}` | 4 条明细（每台 2638.8889），批次合计 = 明细合计 = `10555.5556`，计提凭证在位 |
| ⑮ | 卡片回写 | `GET /asset/{id}` | 累计 `2638.8889`、净值 `97361.1111`、已提 1 月、**`months_left=35` / `amount_left=92361.1111`** |
| ⑯ | 整批红冲 | `POST /depreciation/{id}/reverse {reason}` | 批次 `REVERSED`、红字凭证 `ab4e05fc…`、4 条明细全部 `REVERSED` |
| ⑰ | 红冲回滚 + 重提 | `GET /asset/{id}` + `POST /run {2026-12}` | 卡片回滚到累计 `0.0000` / 净值 `100000.0000` / 已提 0 月；重跑 `outcome=REVERSED_RERUN`、复用同一批次行、合计回到 `10555.5556` |
| ⑱ | 错误码回归 | 红冲原因只填 1 个字 / 处置类型传 `NOPE` | `10001 红冲原因必填（至少 2 个字）：红冲是财务动作，必须说清为什么`、`10001 处置类型只能是 SALE / SCRAP / LOSS（当前 NOPE）` |
| ⑲ | 结算快照 | — | 最终 `FA-20261105-000004` 状态 `IN_USE`、累计 `2638.8889`、净值 `97361.1111` |

补充：为覆盖交互细节 #2，另造一张**已过账收货单**（`18-stockin-create.json` / `19-stockin-post.json`）：
无源单直收 10 件 × 1000 元 + 13% 税 → 提交过账 → fin2gm 批「仓管主管审批」→ `IN-20261105-000002 POSTED`，凭证 `98351ae4…`。

---

## 三、UI 真机验收（vite dev 5199 + Playwright Chromium，29 条断言全绿）

`raw/ui-log.txt` 逐条 PASS；`raw/ui-assertions.json` 结构化结果；截图见下。

| 组 | 断言 | 判定 |
|---|---|---|
| A 台账 | 4 条真数据渲染、合计「共 4 张」 | PASS |
| B 建卡预览（细节 #1） | 空表单是空态文案；填原值 100000 + 分类「电子设备」+ 购入 2026-11-10 → **月折旧额 2,638.89 实时出现**、起提日 `2026-12-01`（购入次月 1 日）、预计提完 `2029-11-30`、尾差月与「round((原值 × (1 − 残值率)) / 月数, 4)」口径文案在位 | PASS ×6 |
| C 收货单转入（细节 #2） | 切「收货单转入」→ 选 `IN-20261105-000002` → 原值预填 `10000.0000` 且 **disabled=true**、进项税 `0.0000` 且 disabled、表单出现「金额来自来源单，已锁定：要改金额请回收货单作废后重开」 | PASS ×4 |
| D 详情抽屉 | 净值 `97,361.11`、已提 `1 / 36`、剩余可提 `35 个月 · 92,361.11`、含变动时间线 / 折旧明细 / 凭证三块 | PASS ×4 |
| E 处置弹窗（细节 #4） | 顶部信息条 净值 `97,361.11` / 已提 `1 / 36` / 剩余可提 `35 个月 · 92,361.11`（全部取接口 `months_left` / `amount_left`）；切「出售」填价 50000 → 损益参考实时 `-47,361.11` | PASS ×4 |
| F 分类页 | 5 条种子（编码 / 名称 / 平均年限法 / 月数 / 残值率 / 三个科目 / 启用）、合计「共 5 条」 | PASS ×2 |
| G 折旧页（细节 #3） | 默认期间（上一个会计月 2026-10）三行汇总：应提 `2,638.89` / 已提 `2,638.89` / 差异 `0.00`＋「账实相符」；切 2026-11 后应提=已提=`7,916.67`、差异 `0.00`；批次明细 3 台、合计 `7,916.67`；红冲弹窗要求原因（灰字说明 + 0/255 计数） | PASS ×6 |
| H 1280 视口 | `t-table__content` `scrollWidth − clientWidth = 0`，状态列右沿 1055 ≤ 固定操作列左沿 1055（**不遮挡**） | PASS |

### 截图（本目录）

| 文件 | 内容 |
|---|---|
| `fe-01-asset-list.png` | 资产卡片台账（4 条真数据，1600 视口，6 个操作一行放下） |
| `fe-02-asset-create-preview.png` | 建卡弹窗 + 右侧折旧预览卡（月折旧 2,638.89 / 起提 2026-12-01 / 提完 2029-11-30）**细节 #1** |
| `fe-03-asset-stockin-locked.png` | 收货单转入：原值 10000.0000 预填 + 只读 + 锁定提示 **细节 #2** |
| `fe-04-asset-detail.png` | 详情抽屉：基本信息 + 处置决策参考 + 变动时间线/折旧明细/凭证 |
| `fe-05-asset-dispose.png` | 处置弹窗：净值 / 已提月数 / 剩余可提 + 损益参考 **细节 #4** |
| `fe-06-asset-category.png` | 资产分类页（5 条种子，科目列 1601 / 1602 / 5602.01） |
| `fe-07-depreciation.png` | 折旧页默认期间三行汇总 **细节 #3** |
| `fe-08-depreciation-2026-11.png` | 切 2026-11：应提=已提=7,916.67、差异 0.00 |
| `fe-09-depreciation-detail.png` | 批次明细抽屉（3 台 + 合计 + 凭证） |
| `fe-10-depreciation-reverse.png` | 整批红冲弹窗（原因必填 + 影响说明） |
| `fe-11-asset-list-1280.png` | 1280 视口台账：无溢出、状态列完整可见 |
| `fe-99-error.png` | 调试期失败快照（保留以示「曾经红过、现已修」；最终一轮无失败） |

---

## 四、四个交互细节的落地口径

1. **建卡实时预览**（`previewDepreciation`，`asset-logic.ts`）：`月折旧 = round((原值 × (1 − 残值率)) / 月数, 4)`，四位 ROUND_HALF_UP、bigint 定点实现；起提日 = 购入次月 1 日；提完日 = 起提日 + 月数 − 1 日；尾差月 = 可折旧总额 − 月折旧 ×(月数 − 1)。100000 / 5% / 36 月 → `2,638.89`、起提 `2026-12-01`、提完 `2029-11-30`（与后端 `asset_compute::monthly_depreciation` 同值）。
2. **收货单转入只读**：`applyStockInSource` 用收货单 `total_amount` 预填 `original_value`、`tax_amount` 强制 `0`，两栏 `disabled`，`help` 换成「金额来自来源单，已锁定：要改金额请回收货单作废后重开」；切回手工录入自动清 `source_id`。
3. **折旧页三行汇总**：应提按「卡片口径逐台测算」（状态 IN_USE/IDLE、起提日 ≤ 期末、累计 < 可折旧总额、处置日 ≥ 期初、`last_depreciation_period < 期间`，单台镜像 `month_amount` 尾差补齐）；已提取该期间 `POSTED` 批次合计；差异 = 应提 − 已提（少提 warning / 多提 danger），并透出 `SKIPPED` 批次原因。
4. **处置弹窗三数**：净值 / 已提月数 / 剩余可提 **全部取 `AssetDetailRes` 的 `months_left`、`amount_left`**，前端不自己算；另给出价 − 净值的损益参考（标注「仅供参考，入账以凭证为准」）。

---

## 五、门禁

| 门禁 | 结果 |
|---|---|
| `pnpm typecheck`（`vue-tsc --noEmit`，干净 worktree HEAD `119e15e`） | **exit 0，0 error 0 warning**（`raw/typecheck.txt`） |
| `tsx --test tests/unit/fin2-asset.test.ts` | **24 / 24 pass**（`raw/unit-fin2-asset.txt`） |
| 全量 `tsx --test tests/unit/*.test.ts` | **759 / 759 pass**（`raw/unit-all.txt`；上一轮记的 `c2-approval-unified` 2 条常红已由 `df74a5f` 收口） |
| 真机 UI | 29 / 29 断言 PASS（`raw/ui-log.txt`） |
| 真机接口 | 19 步链路全通、0 失败（`raw/api-chain-log.txt`） |

---

## 六、口径与已知限制（写清楚，不留含糊）

- **金额一律字符串**：所有 `numeric` 字段后端回 4 位小数字符串（`"2638.8889"`），前端按字符串收、定点 bigint 比较，**不 `Number()` 后比等值**；`salvage_rate` 是 `"0.0500"`（比例，不是百分数）。
- **词表不重复定义**：状态 / 变动 / 处置 / 购入口径 / 批次 / 计提结果统一在 `frontend/src/api/business/erp/asset-kinds.ts`（零 import，便于 `tsx --test` 直跑），`doc-kinds.ts` 里的 `FIXED_ASSET` / `DEPRECIATION` / `ASSET_DISPOSAL` 等原有常量**未改动、未重定义**；后端 `biz_type_domain` 巡检所需的值域一个没加。
- **不在前端算折旧入账**：前端只做展示口径（预览与「本月应提」），入账金额一律以 `POST /depreciation/run` 回包与批次明细为准；红冲/重提后卡片金额以 `GET /asset/{id}` 为准。
- **期间可选范围**：折旧页会计期间下拉是「本月往前 13 个月」（不含未来月），所以 API 验收造的 2026-12 批次在页面上不可选（`fe-07/08` 用的 2026-10 / 2026-11）；这是刻意口径（财务不能给未来月计提），不是缺陷。
- **`REVERSED_RERUN` 批次行保留 `reverse_voucher_id`**：红冲后重跑复用同一批次行（`uk_biz_dep_batch_period` 不被卡死），所以 `db-snapshot.txt` 里 2026-10/11/12 三个 `POSTED` 批次同时带着红字凭证 id —— 与后端 §5.5.5 设计一致。
- **台账列宽**：合计 948px。上一版 1020px 在 1280 视口溢出 52px，固定「操作」列压住「状态」列（实测 `statusRight=1112 > opLeft=1060`）；已按实测收窄到 948px，现 `scrollWidth − clientWidth = 0`、`statusRight = opLeft = 1055`（`fe-11`）。1280 下「名称 / 使用部门」走 ellipsis 是表格既定行为。
- **操作列换行**：台账 6 个动作（详情/编辑/转移/换人/处置/删除）在 1280 视口会折成两行（1600 下为一行），与本仓其它 ERP 列表页（`purchase/index.vue` 的 `.op-cell` 同样 `flex-wrap: wrap`）一致，未额外改交互模型。

---

## 七、复现命令

```bash
# 1) 后端隔离实例（18025 / cloudpivot_fin2asset）
bash scripts/verify/fin2_instance.sh start

# 2) 干净 worktree + 前端 dev（源码即产物，别在 /opt/cloudpivot/frontend 里 build）
git worktree add /tmp/fin2fe-tree 119e15e
ln -sfn /opt/cloudpivot/frontend/node_modules /tmp/fin2fe-tree/frontend/node_modules
cd /tmp/fin2fe-tree/frontend && BACKEND_ORIGIN=http://127.0.0.1:18025 ./node_modules/.bin/vite --port 5199

# 3) 接口全链路（19 步，回包落 docs/evidence/20261105-fin2-fe/raw/）
bash <work>/api-chain3.sh          # 本卡脚本，OUT 可覆盖

# 4) UI 29 条断言 + 截图
cd <work>/ui && ln -sfn /opt/cloudpivot/frontend/node_modules node_modules && node verify.mjs
```
