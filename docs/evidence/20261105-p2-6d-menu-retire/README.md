# P2-6d 证据：软删 `/flow/templates` 菜单节点 + 评审报告 / ARCH-5 口径同步

> 卡：`t_da5318c5`【P2-6d】菜单收口｜依据：`docs/architecture/P2-6-模板实体-决策.md` §3.3–§3.4（**决策 A：合并概念**，已拍板）
> 前置：P2-6c（`t_4dcfa14b`，提交 3ce44c7）已把卡片墙并进 `/flow/def` 的「标准流程」Tab，`/flow/templates` 已 redirect —— 本卡才敢撤菜单入口。
> 时间：2026-11-05 01:39–01:45 CST｜库：`cloudpivot_manual`（真库）+ `cloudpivot_p26d` / `cloudpivot_p26d2`（隔离克隆）

## 一句话

`sys_menu` 里 `path='/flow/templates'` 那一行（id 尾部 `…07a5`，V0194 种下）**置 `deleted_at` 软删**，`/flow/def` 原样保留；迁移号 **V0234**（V0233 被 P2-6b-follow 在途占用 → 顺延，不抢号）；不新增权限码（ARCH-5 规划的 `business:flow:template:read` 随决策 A 取消）。

## 文件

| 文件 | 内容 |
|---|---|
| `backend/migrations/business/V0234__flow_template_menu_retire.up.sql` / `.down.sql` | 软删 / 回滚；up 三条自检（templates 在用=0、def 在用=1、软删不留在用子节点） |
| `scripts/verify/p2_6_template_identity_probe.sh` | P5 组口径翻转：P5a 1→0、P5c 1→0（P5c 改度量「两个在用入口是否并存」）；P5b 改为查含软删行=1（软删≠硬删）；新增 P5d 钉住唯一入口 def=1；文件头写变更理由 |
| `docs/评审报告-现有功能评分-20261104.md` | 功能项 21「流程模板库」→「标准流程（流程定义的预置标记）」（表格行 + §21 + §5.21 + 分档汇总里的一处简称），坑①改写为「已决策收口」并引决策文档 §2.1 |
| `docs/architecture/ARCH-5-工作流引擎设计.md` | §13.2 `business:flow:template:read` 批注**已取消**；§12.4.3 加「数据源 = `biz_flow_def` + `biz_flow_def_version`，不建模板表」；§B 复用清单同口径 |

## 证据文件（本目录）

| 文件 | 证明什么 |
|---|---|
| `migration-isolated-round1.txt` / `migration-isolated-round2.txt` | **隔离库 + 真实迁移跑器**两轮：第 1 轮执行 2 条迁移（V0233/V0234，`234 flow_template_menu_retire ... success=t`），第 2 轮 **0 条**，`deleted_at` 时间戳一字不差 = 幂等 |
| `migration-idempotency-and-manual.txt` | **SQL 级 + 真库**：隔离库与 `cloudpivot_manual` 各做 down→up→up，第 1 轮 `UPDATE 1`、第 2 轮 `UPDATE 0`（可回滚 + 幂等）；随附 `sys_menu` 验收查询与三行计数（templates 在用 0 / def 在用 1 / templates 全部行 1） |
| `probe-run1.txt` / `probe-run2.txt` / `probe-two-rounds-diff.txt` | 探针在 `cloudpivot_manual` 连跑两轮：`P5a=0`、`P5b=1`、`P5c=0`、`P5d=1`、`FAIL=0`；两轮 diff **为空**（只有输出自带的时间戳行不同） |
| `menu-api.txt` | 重启后端 + 登录后的侧边栏真实数据源 `GET /api/system/auth/menus`：顶层 19→**18**、全量 82→**81**，`流程模板库=0`，只剩「审批流 → 流程定义 / 发起审批 / 审批中心」 |
| `invariant-h5-already-merged.txt` | 卡面要求先查的 P2-6a 不变量：**HEAD 里已有**（H5），实跑 6/6 绿 → 只核对，未重复添加 |

## 线上生效顺序（重要）

菜单树是**进程内缓存**：`menu-api.txt` 里 §A/§B 记录了「库已软删但未重启 → 接口仍返回流程模板库」。
所以上线顺序必须是 **① 前端 P2-6c 已发版（有 redirect） → ② 跑本迁移 → ③ 重启后端**。
本卡只把 `cloudpivot_manual` 的库状态改到位；`:8023` 上那个常驻实例是**别的 worker（document-writer）起的**，没有替他重启（它的下次重启会自动幂等应用本迁移并收口菜单）。

## 未做（不在本卡范围）

- 不硬删菜单行、不新增权限码、不动 `model::templates()` / handler / dto（P2-6b）、不动前端页面（P2-6c）。
- `scripts/manual/capture.mjs` 与 `docs/manual/02-审批流.md` 仍用「流程模板库」页面口径 → 已另开跟进卡（使用手册重截）。
