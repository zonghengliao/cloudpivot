# P1-2 业务提醒任务包（应付到期 / 采购超期 / 合同到期 / 低库存）

卡：`t_b5bcc0bd` · 依据：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §6 R-P1-2（断链 DC-06、DC-12 的 `safety_stock` 部分）
范本：`backend/src/business/erp/transfer_overdue.rs`（在途调拨超期告警，LNK-F12）

## 一、交付了什么

| # | 场景 | 判据（唯一真源在代码注释里） | 档位 | 收件人 |
|---|------|------------------------------|------|--------|
| ① | 应付到期 | `biz_payable` 未删除、`status ∈ (OPEN, PARTIAL_PAID)`、`unpaid_amount > 0`、`due_date ∈ {今天+7,+3,+1}` | D-7 / D-3 / D-1 | 供应商对应采购员（采购单制单人 → 收货单制单人 → 应付单制单人逐级回退）+ 财务（`FINANCE_MANAGER` / `FINANCE_DIRECTOR`） |
| ② | 采购超期 | `biz_purchase_order` 未删除、`status ∈ (APPROVED, PARTIAL_RECEIVED)`、`expect_date < 今天` | `OVERDUE-{天数}`（重提醒 3 天） | 采购单制单人 + 采购经理（`PURCHASE_MANAGER`） |
| ③ | 合同到期 | `biz_contract` 未删除、`status ∈ (SIGNED, ACTIVE)`、`end_date ∈ {今天+30,+15,+7,+1}` | D-30 / D-15 / D-7 / D-1 | 合同制单人 + 负责人（`owner_id`）+ 采购经理 |
| ④ | 低库存 | `biz_inventory JOIN biz_material`（启用未删）、`safety_stock > 0`、`qty < safety_stock` | `LOW`（重提醒 3 天） | 仓管主管（`WAREHOUSE_MANAGER`） |

代码落点：

```
backend/src/business/erp/reminder.rs            核心（元数据 / 业务日 / 幂等台账 / 收件人解析 / 发送 / 注册 / HTTP 入口）
backend/src/business/erp/reminder_payable.rs    ① 应付到期
backend/src/business/erp/reminder_purchase.rs   ② 采购超期
backend/src/business/erp/reminder_contract.rs   ③ 合同到期
backend/src/business/erp/reminder_stock.rs      ④ 低库存
backend/src/business/erp/reminder_tests.rs      真库用例（6 条，含幂等三连跑）
backend/migrations/business/V0210__erp_business_reminder.{up,down}.sql
scripts/verify/p12_business_reminder_e2e.py     真机验收（30 条断言）
scripts/verify/p12_migration_roundtrip.sh       V0210 up/down 回放
backend/src/task/mod.rs                         cron 注册 + sys_job 手动执行（run_handler）4 个 invoke_target
```

## 二、幂等设计（本卡重点）

```
第一层（唯一定论）：biz_reminder_log 台账，append-only
    唯一键 (doc_type, doc_id, remind_kind, remind_date)
    remind_date = 业务日（Asia/Shanghai），写回 sent_at = 「已提醒」时间戳
    → 同一份单据 × 同一种提醒 × 同一个业务日只有一行；扫描前一次查完，命中即跳过

第二层（并发兜底）：sys_notice.idempotency_key
    ERP:{KIND}:{doc_id}:{stage}:{yyyy-MM-dd}:{seq}
    seq = 台账历史行数 + 1（重提醒必须换键，否则被 uk_sys_notice_idem 吞掉）
```

**为什么两层都要**：

- 只靠通知幂等键 → `notice_expire_cleanup` 按 `expire_at` 硬删过期通知后幂等键随之消失，
  同一张单会被重新提醒；台账 append-only，与通知生命周期解耦。
- 只靠台账 → 两个实例同时扫都读到「今天没有行」，都会发；通知侧唯一索引吃掉第二条。

**业务日为什么是 Asia/Shanghai**：北京时间 08:00 与 UTC 00:00 差一天，用 UTC 日期会让
「早 7 点的定时提醒」与「下午的手动触发」算成两个业务日 → 一天两条。

**重提醒间隔**：采购超期 / 低库存是状态型（不处理就一直成立），缺省 3 天一次
（`sys_config.erp.reminder.{po_overdue,low_stock}.realert_days`）；档位型（应付 / 合同）
每天最多一次，档位本身保证不会天天重复。

## 三、定时与手动入口

| 任务名（= `sys_job.invoke_target`） | cron（6 段，第 1 段秒） | 说明 |
|---|---|---|
| `erp_payable_due_scan` | `0 10 7 * * *` | 每天 07:10 |
| `erp_po_overdue_scan` | `0 20 7 * * *` | 每天 07:20 |
| `erp_contract_expire_scan` | `0 30 7 * * *` | 每天 07:30 |
| `erp_low_stock_scan` | `0 40 7 * * *` | 每天 07:40 |

排「上班前、整点后 10 分钟」：避开整点的清理任务尖峰，同时保证业务员上班时站内信已到。

手动触发（验收 / 补跑）：

```
POST /api/business/erp/reminder/{kind}/run
kind = payable_due | po_overdue | contract_expire | low_stock | all
权限码：business:erp:reminder:run（V0210 种节点 + 授权 6 个角色）
→ { runs:[{kind,task,scanned,sent,skipped_reminded,skipped_recent,skipped_no_recipient,failed,summary}], message }
```

**sys_job 4 条种子为什么 `status = 0`**：4 个 cron 已由代码内置注册（`Scheduler::register_builtin`，
与 `transfer_overdue` 同范式），种子行只用于「定时任务」页查看 cron 与手动执行一次。
若置 1 会「内置 + 动态」双跑（通知侧有台账兜着不会重复推送，但属白跑）。要用页面开关当运维旋钮，
需先把内置注册摘掉 —— 本卡不做，理由写进了迁移注释。

## 四、门禁证据

| 文件 | 内容 |
|---|---|
| `raw/01-seed-and-cron.txt` | 4 模板 / 4 任务（cron 逐字）/ 1 权限节点 + 授权 / 2 参数 / 台账 10 列 + 幂等唯一索引 |
| `raw/02-real-machine.txt` | 真机 30/30（隔离实例 127.0.0.1:18098 / 库 `cp_p12`），含幂等三连跑、低库存、采购超期、合同到期、sys_job 手动执行、kind=all、400、403 |
| `raw/03-notice-and-ledger.txt` | 真机通知原文（单号 / 关键日期 / 该做什么）+ 收件人行 + 台账行 |
| `raw/04-migration-up-down-up.txt` | V0210 down/up ×2 轮，每轮自检 + 零残留 + 终态 `templates=4 jobs=4 menu=1 cfg=2` |
| `raw/05-full-lib-tests.txt` | `cargo test --lib` 全量（干净库 `cp_p12_full`）|
| `raw/06-invariants.txt` | `scripts/verify/erp_invariants.py` 全量 |

## 五、已知边界（如实说明）

1. **应付只提醒「到期前」不提醒「逾期后」**：卡面口径是「前 7/3/1 天」；逾期催收涉及滞纳金 / 信用政策，
   属另一条业务线，未擅自扩大语义。
2. **「供应商对应采购员」是模型兜底**：`biz_supplier` 没有采购员列（V0137 只有联系人 / 电话 / 邮箱），
   唯一能代表「谁在跟这家供应商」的事实是采购单制单人，因此按
   应付 → 收货单 → 采购单 逐级回退，全空时退回应付单制单人。
3. **低库存按「物料 × 仓库」逐行提醒**：补货是按仓库补的（`biz_inventory` 唯一键就是
   `(material_id, warehouse_id)`）；一轮最多 200 行，重提醒 3 天一次防轰炸。
4. **收件人解析不到人**：不发、记 `skipped_no_recipient` 并 WARN（带收件人规则人话描述），
   不往全员推 —— 提醒推给全员等于没人负责。
5. **合同站内 `PENDING_APPROVAL`（P1-1 引入）不提醒**：还没成为法律事实。
