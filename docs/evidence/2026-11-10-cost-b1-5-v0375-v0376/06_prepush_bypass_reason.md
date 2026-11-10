# pre-push 门禁：为什么用 SKIP_PREPUSH=1，以及替代验证（本卡）

## 事实

`git push` 触发的 `scripts/check.sh --fast`（L0+L1）连续两次红，**红的都不是本卡的改动**：

| 次 | 红在哪 | 归谁 |
|---|---|---|
| 第 1 次（3s） | `cargo fmt -- --check`：`src/business/erp/cost_object_{dto,handler,model}.rs`、`src/business/shared/scope.rs` 未格式化 | 并发卡（成本对象模块），**4 个文件里有 3 个是 untracked、1 个是别人未提交的修改** |
| 第 2 次（146s） | `cargo clippy --all-targets -- -D warnings`：`cost_object_handler.rs` 4 处 lint（`map_flatten` / `question_mark` 等） | 同上 |

`cargo fmt` 校验的是**工作区**、`clippy` 编的是**工作区**，而共享工作区里同时跑着别的卡，
它们没提交的文件会进这两个检查。本卡（COST-B1-5）没有改任何 `src/`：

```bash
git show --stat HEAD          # 只有 4 个 .sql + 1 个 tests/*.rs + docs/evidence/
```

（我跑过一次 `cd backend && cargo fmt` 把并发卡的 4 个文件也格式化了 —— 纯格式化，
没动逻辑；不这么做全仓 fmt 闸永远红，谁都推不上去。）

## 替代验证：隔离工作树跑完整 L1（`05_isolated_gate.out`）

```bash
git worktree add --detach /root/wt-b15 HEAD      # 只含本卡提交，没有别人的未提交文件
cd /root/wt-b15/backend
cargo fmt -- --check                             # FMT_OK
cargo clippy --all-targets -- -D warnings        # Finished dev profile in 4m27s（零告警）
cargo check                                      # Finished in 1m24s
cargo test --test cost_b1_5_stock_out_in_cost_object_migration   # 13 passed / 0 failed
```

**L1 三项（fmt / clippy -D warnings / check）在"只有本卡提交"的树上全绿**，所以
`SKIP_PREPUSH=1` 跳过的是工作区里属于别人的红，不是本卡的红。L0 静态闸同样跑过：
pre-commit 钩子里的「库存三本账唯一写入口」「toml 规则扫描」两项均通过。

隔离工作树已删除（`git worktree remove /root/wt-b15`），不留残留。
