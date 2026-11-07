# FIX-9（t_03122334）145 真机取证 · 审批意见回写 / 凭证摘要 / 审计姓名 / 契约速查表

- **任务**：P4 可读性 11 条里的可修项（① 审批意见回写、② 凭证摘要物料编码、③ 审计姓名快照、④ 角色节点签核口径成文、⑤ 卡面契约速查表）
- **环境**：145 测试机，**隔离实例 8049** + `cloudpivot_dev` 的**拷贝库** `cloudpivot_fix9_20261108030024`
  - 8033 服役实例**全程未动**（收尾 `health=200`，见 `raw/60_teardown.txt`）
  - 144 生产**未触碰**；`cloudpivot_dev` 一个字节未写（拷贝库是 `pg_dump` 备份还原）
- **结论**：三条验收 + 两条口径 **ALL PASS**（`raw/50_e2e_console.txt` 末行 `RESULT: ALL PASS`）

## 一、逐条验收（真机 curl + DB 断言）

| # | 验收项 | 实测 | 证据 |
|---|---|---|---|
| ① | 审批完成 → `biz_flow_instance.result_comment` 有最后一条意见 | 盘点单 `ST-20261108-000002` 提交审批 → 挂实例 `01a1b764-…` → `demo_lead` 审批 `HTTP 200` → 实例 `APPROVED`；实例 `result_comment` = 「FIX-9 ① 审批意见回写验证：实盘差异已核对，同意调平」，与待办行 `biz_flow_task.comment` 逐字相同 | `raw/40_result_comment.txt`、`raw/42_approve_0.json` |
| ② | 凭证摘要显示「存货 物料 DEMO-P101（第 1 行）」 | 无源收货单 `IN-20261108-000002`（`DEMO-P101`，**该物料 `inventory_account_id` 为 NULL**）过账 `HTTP 200` → 分录第 1 行摘要 = `采购入库 IN-20261108-000002 价税合计 339.0000 存货 物料 DEMO-P101（第 1 行）`；全文无 `物料 <8 位 hex>` | `raw/20_summary.txt`、`raw/21_post.json` |
| ③ | `biz_status_log.operator_name` 有姓名 | 同一张收货单 3 行状态日志，`operator_name` 为空 **0 行**：建单 / 提交 = `admin`（令牌路径），过账 = `超级管理员`（**只有 operator_id 的事务体路径 → 主键快照补齐**） | `raw/30_operator_name.txt` |
| ④ | 角色类节点签核行为有文档 | `docs/ERP-审批与审计口径.md` §1：角色节点多成员**默认会签（ALL）**，`approval_mode` 可配 `ANY`（或签）/ `RATIO`；前端「审批模式」下拉可选；`reject_veto` 默认一票否决 | 文档 + 拷库 `biz_flow_node` 实测（`APPROVAL+ROLE` 有 NULL=ALL 与 `ANY` 两种） |
| ⑤ | `docs/api-contract-quickref.md` 覆盖 8 处契约 | 8 条逐条「❌ 卡面写法 → ✅ 真实契约 + 出处」，附无源入库 `in_type` 与折旧汇总口径 | `docs/api-contract-quickref.md` |

## 二、幂等 / 重跑

- **迁移**：本卡**未新增、未修改任何迁移**（`git diff --stat` 只有 5 个 `.rs` + 2 个 `.md`）。
  隔离实例两次启动都是「数据库迁移完成: **应用 0 个迁移**」，`schema_migrations` 264 行、最新 = `V0330`（FIX-7 的），见 `raw/70_idempotent.txt`。
- **同一拷贝库重跑 E2E**：第一轮 `IN-…000001` / `ST-…000001`，第二轮 `IN-…000002` / `ST-…000002`，
  两轮断言全绿 —— 建单 / 过账 / 审批链路可重复执行，不依赖库内既有单据状态（`raw/50_e2e_console.txt` 是第二轮）。

## 三、修前 / 修后对照（判据）

| 现象 | 修前 | 修后 |
|---|---|---|
| 实例详情审批意见 | `result_comment` 恒为 NULL（意见只在 `biz_flow_task.comment`） | 落终态时回写「最新一条非空的 APPROVED/REJECTED 意见」 |
| 凭证摘要 | `存货 物料 01a1b5d2（第 1 行）`（物料**没配存货科目**时编码被一起丢掉） | `存货 物料 DEMO-P101（第 1 行）`（编码与科目解析解耦） |
| 状态日志姓名 | 事务体 / model 路径 `operator_name` 全空（拷贝库全表实测 **132/177 行**为空，全是修复前的历史行） | 按 `sys_user.real_name → username` 补快照；本次 e2e 新写入的 12 行（STOCK_IN / VOUCHER / PAYABLE / STOCK_TAKE）**0 空**，见 `raw/31_operator_name_all_entities.txt` |
| 角色节点签核 | 多成员全员签，无文档 | 口径成文：默认会签，可配 `ANY` 或签 |

## 四、脚本

| 文件 | 作用 |
|---|---|
| `00_env.sh` | 公共环境（BASE/DB/psql 包装/login/断言计数） |
| `10_prepare_145.sh` | `git pull` → `cargo build` → 二进制副本 `cloudpivot-fix9` → `pg_dump`/`pg_restore` 拷贝库 → 前置形状快照 |
| `11_start_instance.sh` | 起隔离实例 8049（`NIK_DATABASE_NAME` 指拷贝库，先按进程名收尸防假健康） |
| `20_e2e.sh` | 三条验收（②凭证摘要 → ③姓名快照 → ①意见回写），curl 真实 token + psql 断言 |
| `14_kill_instance.sh` | 停实例 + 复核 8033 服役实例健康 |
| `21_all.sh` | 一键跑（含 build，**必须后台 setsid**） |

跑法（从 99）：

```bash
ssh root@172.18.10.145 'cd /opt/cloudpivot && \
  setsid nohup bash docs/evidence/20261108-fix9-readability/21_all.sh \
    > /root/backup/fix9_all.log 2>&1 < /dev/null &'
# 轮询 tail -5 /root/backup/fix9_all.log
```

## 五、保留物与清理

- 拷贝库 `cloudpivot_fix9_20261108030024` 保留待复核，确认后人工 drop：
  `dropdb -h 127.0.0.1 -U cloudpivot cloudpivot_fix9_20261108030024`
- `raw/` 内**只有响应体与 DB 查询输出**，无令牌（`grep -rl 'eyJ' raw/` 为空）；令牌落在仓外 `/root/.fix9-tokens`。
