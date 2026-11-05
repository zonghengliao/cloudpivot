# 【P0-FIX-2B】生产组织落位与超管收敛 —— 决策依据（生产只读实测）

> 卡片：`t_357756a7`（architect）｜决策文档：`docs/architecture/P0-FIX-2B-生产组织落位与超管收敛-决策.md`
> 抓取时间：2026-11-05 13:2x CST｜标的：生产 `cloudpivot`@`172.18.10.144`（迁移位点 **246**）
> 性质：**全部只读**（`select` 查询，未做任何写入）

---

## 1. 这里有什么

| 文件 | 内容 | 对应决策 |
|---|---|---|
| `01_prod_accounts_tasks.txt` | 9 个在用账号（部门/锁定/最近登录）、账号-角色-范围、项目 4 行、存活任务按负责人、状态分布、参与的成员关系、角色总表与人数、30 天活跃、`leader_id` | D-1 / D-2 / D-3 |
| `02_prod_pending_flow.txt` | `user_type`/`real_name`/建档人（识别 ROBOT 与残留号）、`biz_flow_*` 表清单、`dept_path`、存量审批实例状态、待办归属 | D-1 / D-4 |
| `03_prod_flow_admin_nodes.txt` | **已发布**流程节点的 `assignee_type` 分布 + `ADMIN` 类节点逐条（74 行） | D-4（最硬的一条） |
| `04_prod_pending_instances.txt` | 2 张 `PENDING` 实例的待办归属（谁在卡单） | D-4 |
| `05_mermaid_exec_order.{mmd,svg,png}` | 决策文档 §7 的执行时序图（mermaid-cli **11.17.1** 实测渲染；`05_mermaid_puppeteer.json` 是本机跑通的 puppeteer 配置） | §7 |
| `sql/*.sql` | 上面四份输出的可复跑 SQL（原样，未二次加工） | — |

## 2. 怎么复跑

生产库口令在 `/etc/cloudpivot/cloudpivot.env`（600，`NIK_DATABASE_PASSWORD`），**不要把口令写进命令历史**：

```bash
# 在 99（工作机）上跑，SQL 从 stdin 送进 144 的 psql
ssh root@172.18.10.144 'set -a; . /etc/cloudpivot/cloudpivot.env; set +a; \
  PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -h 127.0.0.1 -U cloudpivot -d cloudpivot \
  -A -F"|" -f -' < docs/evidence/20261105-p0fix2b/sql/01_accounts_roles_projects.sql
```

只读保险（想更稳就加这一层，PG 会在会话层拒绝任何写语句）：

```bash
ssh root@172.18.10.144 'set -a; . /etc/cloudpivot/cloudpivot.env; set +a; \
  PGOPTIONS="-c default_transaction_read_only=on" PGPASSWORD="$NIK_DATABASE_PASSWORD" \
  psql -h 127.0.0.1 -U cloudpivot -d cloudpivot -A -F"|" -f -' < <(cat docs/evidence/20261105-p0fix2b/sql/03_admin_nodes.sql)
```

## 3. 关键数字（与决策文档一一对应）

```
在用账号 9（含 1 个 ROBOT、3 个锁定 QA 夹具）      → 决策 §1.1
无部门 7 / 有部门 2（qa.limited=TECH、qa.mutex=SALES）→ D-1
组织树 4 个部门，leader_id 4/4 为 NULL             → D-1 / D-6 / N3
biz_task 846 行：dept_id 非空 0｜软删 766｜存活 80   → D-3
biz_project 4 行：4/4 dept_id NULL，owner 全 admin  → D-3
SUPER_ADMIN 4 人｜GENERAL_MANAGER 权限码 12 vs 超管 274 → D-2
已发布流程里 assignee_type='ADMIN' 的节点 5 个（含 1 个会签）→ D-4
PENDING 实例 2 张，卡在 laoliu_test / henry.liao   → §1.7
```

## 4. 踩过的坑（复跑前先看）

- 生产 `cloudpivot` **只在 144**（本机 99 的 5432 上有一堆 `cloudpivot_*` 探针库，**没有** `cloudpivot` 这个库）；
  `ssh root@172.18.10.144` 落地后 `hostname` 与本机同名（都叫 `ubuntu-svr`），别被误导成同机。
- `sys_user_role` **没有** `deleted_at`；有效绑定看 `expire_time`。
- `auth_logs` 的登录结果是 `auth_status`，不是 `status`（写 `l.status` 会报 `column does not exist`）。
- `sys_role` **没有** `is_system`；`biz_project` 的创建人是 `create_by`，不是 `created_by`。
- `biz_flow_def_version` 的流程外键是 `def_id` + `is_current`（不是 `flow_def_id`/`current_version_id`）。
- mermaid 渲染要用本机 playwright 的 chromium（`/root/.cache/ms-playwright/chromium-1248/chrome-linux-arm64/chrome`），
  snap 的 `/usr/bin/chromium-browser` 在 puppeteer 下起不来；配置见 `05_mermaid_puppeteer.json`。
