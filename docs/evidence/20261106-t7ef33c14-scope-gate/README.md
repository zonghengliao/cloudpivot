# t_7ef33c14 · main 上 `scripts/check.sh` 必红（批次B 闸②）收口证据

- 时间：2026-11-06
- 基线：`392ca5c`（main，HEAD）+ 本卡改动
- 卡面现象：`bash scripts/check.sh` 在 main 上必红 —— ① 批次B 闸② 报 5 个文件未接入 scope / 无 SCOPE-EXEMPT，
  且 `SKIP_MANIFEST` 断言报一批「测试面没登记」；② `cargo fmt --check` 在工作区红。

## 根因

1. **`37ce176`（P2-1B：`flow/model.rs` 4732 行纯搬迁拆 10 个部件）只搬了代码，没搬文件头豁免。**
   拆分前整份 `model.rs` 靠文件头的 `// SCOPE-EXEMPT:`（批次D · D1 口径：流程定义是全局配置）过闸②，
   闸② 是**按文件**判定的 ⇒ 拆出来的 5 个部件文件各自都要有自己的标注。搬到 `flow_model_cancel.rs` /
   `flow_model_progress.rs` 的函数级标注保留了，另外 5 个文件（def / graph / instance / node / stats）没有。
2. **`SKIP_MANIFEST` 是口径 v2 硬化 R1 的硬断言**：实测跳过集合与登记清单**多一条少一条都 FAIL**。
   历次模块拆分（erp handler / task handler / auth / flow linkage）新增的测试部件没有同步登记，
   累计 18 条未登记。它不是 main 上「新」坏的，但是同一段 `check.sh` 里一起红的第二个原因。

## 修法（两层，均不含行为变更）

### ① 5 个 flow 部件的逐文件裁定

| 文件 | 查询的登记表 | 裁定 | 依据 |
|------|--------------|------|------|
| `flow_model_def.rs` | biz_flow_def / def_version / edge / **instance** | SCOPE-EXEMPT（文件头） | 定义侧 = 全局配置（D1 口径）；唯一的运行侧表出现在 `templates()` 的 `LEFT JOIN biz_flow_instance`，只聚合「在跑实例数」一列，不返回实例行 |
| `flow_model_graph.rs` | def / def_version / edge / node / node_assignee / **instance** | SCOPE-EXEMPT | 图 / 快照 / 发布全按 `version_id` / `def_id` 主键；`has_running_instance()` 对运行侧只做 `count(*)` 存在性判定（返回 bool） |
| `flow_model_instance.rs` | instance / instance_node / task | SCOPE-EXEMPT | 引擎内部：按主键取数（锁行 `FOR UPDATE`）+ 事务内 `count(*)`，无列表 / 分页，无查看者入参；可见性在 `flow_model_inbox::page_instances`（`scope::filter_for(INSTANCE_TARGET)`）与 handler 单条鉴权 |
| `flow_model_node.rs` | instance / instance_node | SCOPE-EXEMPT | 单行 `LIMIT 1` / `FOR UPDATE`、`max()` / `count(*)` 单值、`nodes_of` 按已鉴权 `instance_id` 扇出子行 |
| `flow_model_stats.rs` | instance / instance_node | SCOPE-EXEMPT（**机制登记**，非放弃权限） | 两条统计查询已调 `instance_visibility()`（`flow_model_inbox.rs:115`，内部 `scope::filter_for(INSTANCE_TARGET)` 并与「我发起 ∪ 我待办/已办」取并集）；闸② 只搜本文件里的 `scope::` 字面量，故显式登记并写明「删掉 `instance_visibility()` 才是违规」 |

> 口径说明：闸② 是**文件级**放行（口径 v2 附录 R7 已登记该半径）。本次没有新增豁免类别、
> 没有放宽判据，只是把拆分前的文件头豁免按部件复位，并把每个部件的**具体理由**写在文件头。

### ② `SKIP_MANIFEST` 补登 18 条测试面

逐条核对过程与证据见 `03_skip_manifest_逐条核对.md` / `03b_未登记清单核对_原始输出.txt`；
核对工具 `scripts/verify/scope_skip_manifest_audit.py`（四条判据：声明处 `#[cfg(test)]`、`#[test]` 条数、
`pub fn` 为 0、引用者只限测试面/父模块）。

## 验证（全部真实执行）

| # | 动作 | 结果 | 证据文件 |
|---|------|------|----------|
| 1 | HEAD 版门禁脚本跑当前树（复现） | **exit 1**：5 个文件未接入 + 18 条未登记 | `01_gate_before_HEAD门禁脚本.txt` |
| 2 | 修后门禁脚本跑当前树 | **exit 0**：扫描生产面 378 / 跳过测试面 79，`PASS：跳过清单与 SKIP_MANIFEST 逐条一致（79 条）`、`PASS：0 处` | `02_gate_after.txt` |
| 3 | 门禁自检 `--self-test`（12 组红/绿语料） | 全部 PASS（含 R1 跳过清单断言、R3 字符级跳过、防假绿 guard） | 见下「命令」 |
| 4 | 清单审计工具（当前树） | `未登记 0 条；登记失效 0 条`，exit 0 | — |
| 5 | `bash scripts/check.sh`（工作区） | **exit 0「==> 全部检查通过」**，`批次B 门禁汇总：闸①=0 处 闸②=0 处` | `04_check.sh_整体跑通.txt` |

复跑命令：

```bash
cd /opt/cloudpivot
python3 scripts/lint/scope_gate.py --root . --self-test
python3 scripts/lint/scope_gate.py --root . --baseline 21
python3 scripts/verify/scope_skip_manifest_audit.py --root .
bash scripts/check.sh                       # 后端 fmt/clippy/check + 前端 typecheck + 闸⓪–⑧
```

## 关于卡面的 ②「`cargo fmt --check` 红」

取证时（`11_check.sh整体跑通.txt`）那条 diff 来自**别人未提交**的
`backend/src/business/flow/edge_admin_tests.rs`（卡面也这么判定）。本次跑 `check.sh` 时该文件已 fmt 干净，
`==> 后端：cargo fmt --check` 直接通过 ⇒ main 上不存在这条红，不需要为它做任何改动。

## 遗留 / 给后续拆分卡的口径提醒

- 模块拆分（`cargo` 文件级搬迁）**必须同步**两件事：① 每个新部件自己的 `SCOPE-EXEMPT` / `scope::`；
  ② 新测试部件登记进 `scripts/lint/scope_gate.py` 的 `SKIP_MANIFEST`。缺任何一件，`check.sh` 会在
  闸③–⑧ 之前中断，「谁跑都拿不到自己那段的结果」。
- 建议拆分卡把这两条写进验收判据，而不是靠事后 red 发现。
