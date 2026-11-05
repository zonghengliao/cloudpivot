# E2ERES3 · 清掉生产库里的 e2e 标记残留（383 行 + 6 个在用测试账号）

一次性生产数据修复（2026-11-05，卡 `t_44766ab3`）。**不是迁移**，不进 `backend/migrations/`。
承接 `scripts/datafix/e2e_residue/`（`t_61dca492`）、`scripts/datafix/e2e_residue2/`（`t_142317a2`）。

前两张卡清的是「流程定义 / 流程实例」（`biz_flow_def` / `biz_flow_instance`）。
本卡清的是**业务侧可见的东西**：任务、任务评论、通知、公告、上传件、导出任务、帮助分类，
以及 6 个「还在启用状态」的测试账号。

## 生产实测（144 / `cloudpivot`，2026-11-05 13:41 只读盘点）

| 表 | 活跃命中 | 全量（含已软删） | 样例 |
|---|---|---|---|
| `biz_task` | **9** | 355 | `[E2E]详情-e2emvvvdf01…` |
| `biz_task_comment` | **221** | 221 | 挂在上面 9 个任务下 |
| `sys_notice` | **105** | 108 | `待你审批：[E2E]审批单-…` |
| `sys_file` | **43** | 45 | `e2e-e2emvugheec.txt` |
| `sys_export_task` | **2** | 2 | `e2e_export` |
| `biz_flow_instance` | **1** | 1 | `TSK-… [E2E]探针-…` |
| `sys_announcement` | **1** | 2 | `E2E公告测试 085522`（**已发布**） |
| `sys_help_category` | **1** | 2 | `e2e_cat_<stamp>` |
| `biz_warehouse` | 0 | 4 | 4 个 `E2E-W4A-*` **早已软删** → 确认干净，本包不动 |

合计 **383 行**活跃标记数据。测试账号 6 个（`cl_43870` / `laoliu_test` / `qa.limited` /
`qa.mutex` / `qa.viewer` / `verify-ai`）全部 `deleted_at is null AND status = 1`。

## 处置口径（与卡面验收逐条对应）

| 对象 | 动作 | 为什么 |
|---|---|---|
| E2E 任务及其从属行（评论/通知/公告/上传件/导出/帮助分类/流程实例） | **软删** `deleted_at = now()` | 保留行与审计；客户看不到，事后可查可回滚 |
| 6 个测试账号 | **禁用** `status = 0`，**绝不删行** | 历史 `create_by` / 审计引用指向它们，删了操作日志的主人就「查无此人」 |
| E2E 仓库 | **不动**（已软删） | 确认干净，本包不承担删它 |
| `update_by` | **不改** | 谁动的库由快照 + 账本 + git commit 交代；伪造业务操作人更糟 |

## 判据：唯一实现是 `scripts/verify/e2e_marker.py`

本包的 SQL 判据（`10_precheck.sql` 里的视图 `datafix_e2eres3_spec`）是它的**SQL 镜像**，
`apply.sh` 结尾会再跑一次 `e2e_marker.py --assert-clean`：**本包自认为清干净了、
判据脚本也认干净，才叫清干净**（只按宽的那份绿 = 假绿）。

为什么不是「一个 `%E2E%` 全表扫」：生产库里真实内容提到 e2e 的有 6 条
（`laoliu` 的 4 条开发汇报挂在 `T8-A`/`P4-B` 等真实任务上、`verify-ai` 的 2 条真实阻塞通知），
按正文匹配会误删。所以：**任务评论的判据是「父任务带标记」，通知的判据是「标题带标记」**，
不看正文。反例钉在 `e2e_marker.py --self-test` 的 `MUST_NOT_MATCH` 里。

## 三道闸（都不靠记忆）

| 闸 | 在哪 | 拦什么 |
|---|---|---|
| ① 备份闸 | `00_backup.sh` → `apply.sh::find_backup` | MANIFEST 里必须是**本库**的备份；拿别库备份糊弄不过去 |
| ② 判据闸 | `apply.sh` 末尾 | `e2e_marker.py --assert-clean` 不认干净 → 红 |
| ③ 生产写保护 | `apply.sh::assert_db_writable` | 与 `scripts/verify/db_guard.py` 同一套规矩：生产库必须 `E2E_ALLOW_PROD=1` + `E2E_PROD_REASON='<谁批准/为什么>'`，放行后留痕 `/tmp/e2e-prod-target.log`；演练库自动放行 |

## 用法

```bash
# 0) 在 144 上（生产机）
cd /opt/cloudpivot
set -a; . /etc/cloudpivot/cloudpivot.env; set +a

# 1) 演练（克隆生产库 → up → verify → down → verify → up×2 幂等；不动生产）
KEEP_DRILL_DB=1 bash scripts/datafix/e2e_residue3/drill.sh

# 2) 生产：备份 → 应用
bash scripts/datafix/e2e_residue3/00_backup.sh
E2E_ALLOW_PROD=1 E2E_PROD_REASON='<谁批准/为什么>' bash scripts/datafix/e2e_residue3/apply.sh

# 3) 复查 / 回滚
bash scripts/datafix/e2e_residue3/apply.sh --check
E2E_ALLOW_PROD=1 E2E_PROD_REASON='<…>' bash scripts/datafix/e2e_residue3/apply.sh --rollback

# 4) 不变量（O1：生产库不许有活跃 E2E 标记行 / 在用测试账号）
python3 scripts/verify/erp_invariants.py --group E2E
```

`E2ERES3_DB=<库名>` 可指定目标库（演练库用这个切）；不设则用 `NIK_DATABASE_NAME`。

## 文件

| 文件 | 作用 |
|---|---|
| `00_backup.sh` | 10 张表整表 `pg_dump -Fc` + 命中行 CSV + 账号原值 + MANIFEST（闸① 认它） |
| `10_precheck.sql` | 只读断言（真实子任务不得挂在 E2E 父任务下 / 不得有在飞审批 / E2E 实例不得被真实单据引用）+ 建判据视图与快照账本 |
| `20_softdelete_e2e_residue.up.sql` | **单事务**：先整行快照，再软删、再禁用账号 |
| `20_softdelete_e2e_residue.down.sql` | 按快照逐行写回（不重算判据） |
| `30_verify.sql` | 自检：残留=0 / 6 个账号 status=0 且行还在 / 账务与审计表行数不变 / 快照覆盖 |
| `apply.sh` | 三道闸 + 应用/回滚/现状 |
| `drill.sh` | 克隆生产库，跑 up→verify→down→verify→up×2，含**反向对照**（回滚后 30_verify 必须红） |

## 留下的对象（有意为之）

`datafix_e2eres3_spec` / `_snapshot` / `_guard` 三张表留在生产库：
- `_snapshot` 是回滚的唯一依据，也是「这一批动过哪些行、当时长什么样」的审计账；
- `_guard` 记了每张表删了多少、改前账务行数、自检结论；
- `_spec` 是判据视图，`30_verify.sql` 复跑要用。

它们与迁移无关、不带业务语义，`\dt datafix_*` 一眼能看出是数据修复的痕迹。
