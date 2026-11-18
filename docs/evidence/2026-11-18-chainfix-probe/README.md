# 证据：`task_chain_valid.py --fix` 在「2 张游离清债类卡」形态下**不收敛，且会把主链打散**

- 发现于卡 `t_1d66ac6d`（补 SKIP_MANIFEST 38 条）：该卡跑完前按团队约定要给新立的 2 张
  【测试卫生·门禁】卡串链，`--fix --dry-run` 打印 **200 处调整**且同一条反复出现 → 不敢真跑，
  改手工落边；事后在**副本库**上真跑 `--fix` 复现「链被打散」。
- 时间：2026-11-18；HEAD `fb16fcd5`；DB `/root/.hermes/kanban/boards/dev/kanban.db`（WAL 模式）。

## 复现（全部在**副本**上做，真库未动）

```bash
LIVE=/root/.hermes/kanban/boards/dev/kanban.db
cp $LIVE chainprobe.db; cp $LIVE-wal chainprobe.db-wal; cp $LIVE-shm chainprobe.db-shm
# 把 2 张卡恢复成「游离 + ready」
python3 - <<'PY'
import sqlite3
con=sqlite3.connect("chainprobe.db")
with con:
    for c in ("t_71d2aa63","t_a5eca17f"):
        con.execute("DELETE FROM task_links WHERE child_id=? OR parent_id=?",(c,c))
        con.execute("UPDATE tasks SET status='ready' WHERE id=?",(c,))
PY
HERMES_KANBAN_DB=chainprobe.db python3 scripts/lint/task_chain_valid.py --fix --dry-run  # ① 干跑
HERMES_KANBAN_DB=chainprobe.db python3 scripts/lint/task_chain_valid.py --fix             # ② 真跑
HERMES_KANBAN_DB=chainprobe.db python3 scripts/lint/task_chain_valid.py --verbose         # ③ 看终局
```

> 注意：`cp kanban.db` 单文件在 WAL 模式下会得到**旧快照**（连新卡都不在），必须先复制
> `-wal` / `-shm`，否则会误判成「结构完整」。

## 观测

| # | 文件 | 结果 |
|:--|:--|:--|
| ① | `01_dry_run_200_no_converge.out` | `共 200 处调整` 不收敛；**198 行「插在链头」**全是同一张卡 `t_a5eca17f` |
| ② | `02_real_fix_shreds_chain.out` | 真跑同样 200 轮不收敛；rc=1 |
| ③ | `03_after_real_fix_chain_1_card.out` | 终局：**`主链 1 张，未完成 112 张`** —— 整条链从链头已走不到，等于链被打散；`t_a5eca17f` 仍 C3+C4 |

真库对照（手工落边后）：`✓ 任务链结构完整`。

## 根因（读代码定位，`scripts/lint/task_chain_valid.py`）

1. `Chain.head()`（第 170-180 行）的优先序是**「未完成卡里唯一没有父卡的那张」先于 `running`**：

   ```python
   no_parent = [t["id"] for t in self.open_tasks() if not self.parents.get(t["id"])]
   if len(no_parent) == 1:
       return no_parent[0]      # ← 2 张游离卡被修掉 1 张后，剩下的那张**成了链头**
   r = self.running()
   ```

2. `apply_fix` 把第一张游离卡（`t_71d2aa63`）接进链后，**只剩 `t_a5eca17f` 一张无父卡** →
   `head()` 返回它 → `walk()` 只走得到它自己 → `order_wo = []`。
3. `choose_insert` 在 `order_wo == []` 时被钳成 `idx = 0` → `prev_node = next_node = None` →
   第 368 行先 `DELETE FROM task_links WHERE child_id = card`（**把它已有边删了**），
   第 373-391 行因 prev/next 皆 None **一条边都不建** → 状态没变 → 下一轮重来，转 200 轮。
4. 每轮都 `con.commit()`（第 408-409 行），所以**破坏是落库的**；而且循环里没有任何
   「本轮是否真的减少了违规数」的判据，只有 `guard < 200`。

## 建议改法（按「判据只加不减」）

1. `head()`：`running` 存在时**一律以 running 为链头**（它是权威起点），
   「唯一无父卡」只在没有 running 时兜底。这一条单独就能让本形态收敛（running 恒为当前卡）。
2. `choose_insert`：`order_wo` 为空 / `prev` 与 `next` 同时为 None 时**直接返回失败**，不许
   `idx = 0` 静默通过。
3. `apply_fix`：**先算后改** —— 算出合法的 `(prev, card, next)` 三元组之前不许 `DELETE`；
   每轮断言违规数严格下降，否则 rollback 并报「无法自动修复，请人工落边」（给出可粘贴的
   `INSERT INTO task_links` 语句）。
4. `--dry-run` 目前会把每条计划打印成「200 处调整」的重复行，也应一并修（干跑要能当证据用）。

## 影响

- 团队约定就是「建卡后跑 `--fix` 把新卡串进主链」。这条路径在本形态下会**毁掉整条链的边**，
  而边是调度器判「父子解锁」的唯一依据 —— 打散后子卡会被当成可派发，或永远不派发。
- 该脚本是 L0 静态闸（`check.sh`）的一项，`--fix` 坏了但检查模式正常，所以门禁不会报警，
  只有真跑 `--fix` 的人会踩。
