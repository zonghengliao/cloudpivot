# HR-45 证据：收口自检迁移 V0398 + 人力资源巡检脚本（10 断言）

- 卡：`t_914199ef`（HR-45，批 4 薪资，依赖 HR-39）
- 设计：`docs/architecture/ARCH-9-人力资源模块设计.md` §4.9（10 条断言）、§7.1（`hr_selfcheck`
  迁移）、§8.1（本卡）
- 环境：**145**（172.18.10.145）演练库，`hr45_drill3`（跑完已 DROP；99 上未建任何库）

## 交付物

| 文件 | 作用 |
|---|---|
| `backend/migrations/business/V0398__hr_selfcheck.up.sql` | 收口自检：一个纯 `DO` 块，8 组种子（10 表 / 9 科目+4 父级翻非末级+2 凭证规则 / 4 号段+4 字典 26 项 / 36 菜单+`HR_MANAGER` 授权 / 2 脱敏+3 配置 / 4 流程 / 17 薪资项）缺一项就 `RAISE EXCEPTION` 并**一次性列出全部缺项** ⇒ 整批迁移回滚 |
| `backend/migrations/business/V0398__hr_selfcheck.down.sql` | 显式「空回滚」（本迁移只读，无可反向状态；种子的回滚归 V0383~V0397 各自的 down，它们带业务数据护栏） |
| `scripts/verify/hr_invariants.sql` | §4.9 的 10 条断言（A1~A10：8 BLOCKER + A4/A5 WARN）+ W1/W2 两条 WARN 观察项；**只读**，只建一张 `pg_temp` 结果表 |
| `scripts/verify/hr_invariants.sh` | 巡检入口：解析连接串 → 以 `report_only=1` 跑完 12 条 → 读 `HR_INVARIANTS_SUMMARY` 定退出码（0/1/2） |
| `scripts/verify/hr_invariants_selftest.sh` | 造错自检：R0 全绿零误报 + I1~I10 逐条注入验证「对应断言必红」+ F1 裸跑 fail-closed；**演练库护栏**（库名需含 drill/e2e/test）、每轮 `BEGIN→夹具→造错→ROLLBACK` 零残留 |
| `scripts/verify/hr/hr_invariants_fixture.sql` | 全绿基线夹具：员工→合同→薪资标准→考勤期/台账→请假单+审批实例→已过账工资单（明细+明细项）→计提/发放两张凭证 |
| `scripts/verify/hr/hr_db.sh` | 连接解析（`HR_DB_URL` > `ERP_DB_URL` > `NIK_DATABASE_*` > `PG*`）+ 演练库护栏 |
| `backend/src/business/hr/hr_invariants_tests.rs` | 7 条静态契约测试（断言条数与级别、巡检只读性、造错自检覆盖面、V0398 纯 DO / down 存在 / 版本号唯一） |

## 实测（原始输出就在本目录）

| 文件 | 内容 | 结论 |
|---|---|---|
| `01_invariants_empty_db.out` | 空库巡检 | 12 行全 0，`blocker_fail=0`，rc=0 |
| `02_selftest.out` | 造错自检 | R0 零误报；I1~I10 十条断言逐条 viol≥1；F1 裸跑 rc=3（fail-closed）；总 rc=0 |
| `03_selfcheck_migration_teeth.out` | V0398 幂等 + down + 10 项造错 | 连跑两遍 up RC=0 同结果；down RC=0；10 个破坏项**全部**报红并点名缺什么；回滚后零残留 |
| `04_fresh_replay_and_selftest.out` | 145 空库全量回放 + 巡检 + 造错自检 | 319/319 条迁移通过（含 V0398）；巡检 PASS；自检 PASS |

## 关键数字：319 条迁移在空库跑通

`hr45_drill3` 从空库按版本号升序回放 `backend/migrations/**/V*__*.up.sql` 共 319 条，
含本卡的 V0398 ⇒ 证明收口自检在**全新安装 / 灾难重建**路径上不会因缺种子而误拦。

## 造错自检的 10 个注入（每条只破坏「目标断言」的前提）

| # | 注入 | 目标 | 实测 |
|---|---|---|---|
| I1 | 员工档 `user_id` 指向不存在的账号 | A1 | viol=1 |
| I2 | 先临时 `DROP INDEX uk_biz_employee_user` 再插第二份员工档 | A2 | viol=1 |
| I3 | 同员工同薪资项追加一段重叠生效区间 | A3 | viol=2（两两相交） |
| I4 | 软删唯一一份有效合同 | A4（WARN） | viol=1 |
| I5 | 员工改离职且离职日晚于合同到期日（`status=RESIGNED` 时 A4 不再适用） | A5（WARN） | viol=1 |
| I6 | 明细 `earning/gross/net` 各 +100（保持库层 CHECK），单头不动 | A6 | viol=1 |
| I7 | 明细项 `BASE` +50，明细汇总不动 | A7 | viol=1 |
| I8 | 已过账单据的 `payment_voucher_id` 清空 | A8 | viol=1 |
| I9 | 计提凭证期间挪到 1999-01（期间表无此行） | A9 | viol=1 |
| I10 | 请假单改 `APPROVED` 但实例仍 `PENDING` | A10 | viol=1 |

注入里有两处「为了注入能落库」的妥协，都写在脚本注释里：
- I2 需要临时 `DROP INDEX` —— 库层唯一索引是兜底，注入要证明的是「巡检能**独立**发现」，
  不是「巡检靠索引」；
- I6 要同时改三个列 —— `ck_biz_sal_det_net`/`ck_biz_sal_det_gross` 会拦住只改一列的注入。

## 与设计字面的两处具名偏差（详见 `hr_invariants.sql` 头注释与设计 §4.9 落地记录）

1. **A9**：设计原文「凭证 period 必须 `status <> 'CLOSED'`」在正常月结后会**永久红**（结账是
   常规动作）。改为「凭证期间必须存在」+「已结账期间里不许留 `SUBMITTED`/`APPROVED` 的死单」
   —— 后者才是设计要防的「结账后还能改数」，且不产生误报。
2. **A10**：设计原文「APPROVED 的必须有已通过的节点」在**免审直过**（流程无审批节点、
   引擎同事务直接 APPROVED）时没有节点行。改为断言实例终态 = `APPROVED`。

## 复现

```bash
# ① 145 建演练库（库名必须含 drill/e2e/test）
DBPW=$(ssh root@172.18.10.145 'grep "^NIK_DATABASE_PASSWORD" /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-')
PGPASSWORD=$DBPW psql -h 172.18.10.145 -U cloudpivot -d postgres -c "CREATE DATABASE hr45_drill3"
export HR_DB_URL="postgres://cloudpivot:$DBPW@172.18.10.145:5432/hr45_drill3"

# ② 回放全部迁移（含 V0398）+ 巡检 + 造错自检
bash /path/to/04 里的回放脚本 hr45_drill3      # 或见 evidence 里的脚本 05_replay.sh
bash scripts/verify/hr_invariants.sh; echo $?
bash scripts/verify/hr_invariants_selftest.sh; echo $?

# ③ 清理（必须）
PGPASSWORD=$DBPW psql -h 172.18.10.145 -U cloudpivot -d postgres -f /path/to/06_drop.sql
```
