# check.sh 在干净树上从「首闸即红」到「全部检查通过」——修复与复验实录

卡：`t_56e57909`（来源卡 `t_e7c3cf7d` 的全量回归：`bash scripts/check.sh` 在干净基线
`9867fbe` 上第一道闸（cargo fmt）就断，后面 8 道闸一条都没执行）。

修复提交：`47043b6`（main，**仅本地，未推 144 bare**；5 文件 +31/−12）。

## 1. 修复前（9765447，`git archive` 出来的纯提交内容，0 未提交文件）

| 闸 | 结果 | 证据 |
| --- | --- | --- |
| cargo fmt | exit 1，2 文件 3 处 | `before-fmt.log.txt` |
| 批次B 闸② | exit 1：4 条 SKIP_MANIFEST 未登记 + 2 个 file×table 未接 scope:: | `before-gate2.log.txt` |

`before-fmt.log.txt` 里 `doc_kind_tests.rs` 已经 exit 0 —— 卡面「顺带确认」这一条成立：
9ed4bf8 时代它红，现树不再需要动。

## 2. 修复内容（47043b6）

1. **纯格式化**（`cargo fmt` 产出，未手改，语义零变化）：
   - `backend/src/bootstrap.rs:112` — `tracing::info!` 长串折行
   - `backend/src/shared/startup_guard.rs:969` — `format!` 长串折行
   - `backend/src/shared/startup_guard.rs:1059` — `assert!` 内结构体字面量重排
2. **`scripts/lint/scope_gate.py` SKIP_MANIFEST +4 行**（四个都是 `#[cfg(test)]
   #[path = "…_tests.rs"] mod …;` 挂载的真测试面，逐文件确认内容全是 `#[test]` /
   fixture，无生产代码）：
   `asset_model_tests.rs` / `budget_dto_tests.rs` / `budget_guard_tests.rs` / `budget_model_tests.rs`
3. **闸② 另外那 2 个 file×table 红点**（卡面没点名，但不修 check.sh 仍然停在闸② →
   走不到后面的闸）：`asset_handler.rs` / `budget_handler.rs` 各有「按主键取单行显示名」
   的查询（`dept_name_of` / `user_name_of` 查 `sys_dept` / `sys_user`）。这类查询注入
   数据范围反而会让「别的部门的单据」查不到自己挂的部门名，按闸② 既有口径 ② 写
   `// SCOPE-EXEMPT: <理由>`（仓库已有 6+ 处同款先例：`changelog/model.rs:257`、
   `aiagent/model.rs:109`、`announcement/model.rs:2041` 等）。
   注：闸② 的 SCOPE-EXEMPT 是**文件级**判据（gate 文档写明「沿整文件搜索」），
   所以这 2 个文件今后若新增登记表列表查询不会被拦住 —— 评审时请按此口径看。

**没有**用 `BATCHB_GATE=warn` 绕行：那是取后续闸结论的临时手段，不是收口方案。

## 3. 修复后复验（隔离 `git clone`，0 未提交文件）

```
cd backend && cargo fmt -- --check        → exit 0
bash scripts/check.sh                     → exit 0「==> 全部检查通过」
```

两份日志：
- `check-sh-clean-47043b6.log.txt`（103 行）—— 修复提交本身的首次全链复验（私有 target，
  clippy 因换了工作区路径要从头编 vendor 依赖）。
- `check-sh-clean-71dcaca.log.txt`（84 行）—— **最终 HEAD**（47043b6 + 证据提交）再跑一次
  的全链复验，target 已热，exit 0。

逐闸结果：

| 闸 | 结果 |
| --- | --- |
| cargo fmt --check | PASS（exit 0） |
| cargo clippy --all-targets -- -D warnings | PASS（`CARGO_BUILD_JOBS=2`，私有 target） |
| cargo check | PASS（43.41s） |
| pnpm typecheck（vue-tsc --noEmit） | PASS |
| 批次B 闸① | 0 处 |
| 批次B 闸② | PASS（清单 61 条逐条一致 / 0 处越界） |
| C7 G1–G6 | 6/6 PASS（G5 检查 29 个 record 调用点） |
| 色板一致性 | 22 套逐字一致 |
| 库存三本账唯一写入口闸 | `--self-test` 22/22 + 主闸「零越界」（341 生产文件 / 豁免 61） |
| 闸③ ERP 不变量 / 闸④ 词表值域 / 闸⑤ 快照巡检 | SKIP（无 `NIK_DATABASE_PASSWORD` / `PGPASSWORD`，属正常） |

另：`python3 scripts/lint/scope_gate.py --self-test` → 12 条负例全 PASS（改常量后回归自测仍绿）。

## 4. 遗留（不属于本卡，已告知）

工作区里 `backend/src/business/erp/expense_model_tests.rs`（FIN-3 在飞、尚未提交）在
**脏工作区**里同样会被判成测试面而缺登记。它在提交树上不存在，所以现在**不能**预登记
（清单断言「多一个少一个都放行不了」，预登记会让干净树 FAIL）。FIN-3 提交该文件时
必须同时把 `business/erp/expense_model_tests.rs` 补进 `SKIP_MANIFEST`。

同理：`/opt/cloudpivot` 共享工作区当前还有别人未提交的 `expense_*.rs` 等文件，
在**脏工作区**跑 `check.sh` 仍会在 fmt 处红 —— 那是那些卡的施工状态，不是本卡回归。
本卡的验收面是「已提交树 + 0 未提交文件」，与卡面 `git clone --no-checkout` 的复现方式一致。
