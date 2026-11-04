# P2-8 证据包 · 项目模块现状（只读普查 + 接口口径复算）

对应决策文档：`docs/architecture/P2-8-项目模块定位决策.md`
卡片：`t_7249911b`（product-manager）｜采集时间：2026-11-04 22:15–22:22 CST

## 文件

| 文件 | 作用 |
|------|------|
| `survey.sh` | 全机业务库普查：遍历所有 `biz_project` 库，统计项目/空壳项目/任务/工时/成员行数；再看里程碑/预算/权限码/菜单的能力存量；最后看「挂在项目任务身上的工时」是否真跑通过 |
| `run1.txt` | 普查第一轮（本卡前一次尝试 22:12） |
| `run4.txt` | 普查最新一轮（本卡 22:19，脚本未改）——与 run1 的差异仅为并行卡新建的测试库（`cloudpivot_4b`、`cloudpivot_9a6e3d35_diag`、`9a6e3d35` 多 1 项目），**`member_rows=0` 两轮一致** |
| `sql_recompute.sql` | 把 `handler::page` + `model::stats_for_projects` 的口径用 SQL 原样复算（成员数 / 任务数 / 完成数 / 预估工时 / 实际工时 / 项目维度工时明细行数） |
| `recompute_two_rounds.sh` | 连跑两轮并 diff（验收要求「连跑两轮」） |
| `run-A.sqlrecompute.txt` / `run-B.sqlrecompute.txt` | 两轮复算输出（现测逐字一致，16 行） |
| `api_probe.py` | 只读接口探针（登录 + 项目分页/详情/成员）。登录返回 401（本机 dev 库 admin 口令非仓库默认值）→ 按「不猜凭据」直接 SKIP，不写任何数据 |
| `run-A.api_probe.txt` / `run-B.api_probe.txt` | 探针两轮输出（一致） |

## 复跑

```bash
# ① 两轮一致性（必须 RESULT: 两轮逐字一致）
bash docs/evidence/20261104-p2-8/recompute_two_rounds.sh

# ② 全机普查（约 44 秒）
bash docs/evidence/20261104-p2-8/survey.sh > docs/evidence/20261104-p2-8/run5.txt
```

## 读结论只看这三行

```
member_rows=0                           # 254 个库、526 个项目，成员行 0 条
table:%milestone%=f / biz_budget=f      # 里程碑、预算在项目侧都不存在
worklog_rows_on_project_tasks=0（真实库）# 唯一非 0 的 c_task_v2 是没有 biz_project 的压测夹具
```

## 已知限制

- 写路径（加/删成员）未做 E2E：需测试库 + 全量编译，且会与并发水位抢资源；证据改为「集成测试 P04 覆盖写入」+ 代码路径审查。K2 交付时必须补真机验证。
- `run-A.sqlrecompute.txt` 里的时间戳在首行，diff 用 `tail -n +2` 跳过。
