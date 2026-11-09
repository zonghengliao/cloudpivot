# NEAR-FIX-2 证据包（卡 t_2746ca53 · 门禁加固）

**commit**：`b876c6d0` feat(gate): 门禁分层 L0/L1/L2/L3 + pre-push 调 check.sh --fast + 干净检出闸
**基线**：`5cb41fca`（起手 HEAD）

## 验收逐条对照

| # | 验收项 | 结果 | 证据 |
|---|---|---|---|
| ① | `.githooks/pre-push` 已装且可执行 | ✅ | `05_hooks_check.out`（install_hooks.sh --check：三钩子可执行、hooksPath=.githooks、exit 0） |
| ② | 正常 push 通过 | ✅ | `04_prepush_normal.out`：`5cb41fca..b876c6d0  main -> main`，pre-push 40s |
| ③ | 造 clippy 告警 → push 被拒 | ✅ | `03_prepush_clippy_red.out`：`error: unused variable` → `✘ push 被拒绝`（退出码 101）→ `error: failed to push` |
| ④ | check.sh 支持 --fast/--full | ✅ | `07_check_fast.out`（--dry-run 计划 + --fast 40.5s 绿）、`01/02_check_full.out` |
| ⑤ | 干净检出能跑配置相关测试（不是必红） | ✅ | `06_clean_checkout_gate.out`：干净树 4032 passed / 0 failed（5.4s），自检 5/5 |
| ⑥ | AGENTS.md 或 docs/编码规范.md 有门禁说明 | ⚠️ 部分 | `docs/编码规范.md §3`（钩子表/安装/绕过/四层/禁止本机依赖）已重写；**AGENTS.md 是受保护文件**，本次无人在场取不到批准 → 待用户批准后补（改动内容见卡内 comment） |
| ⑦ | push | ✅ | `04_prepush_normal.out`（`ls-remote` 复核见下） |

## 关键实测数字

| 项 | 数字 | 环境 |
|---|---|---|
| `check.sh --fast`（L0+L1，全缓存） | **40.5 s** | 99，4 核 / 15 G |
| 其中 pre-push 整条链路 | 40 s | 同上 |
| `check.sh --full`（L0–L3，无 DB 口令 → L2 连库档跳过） | 49.7 s | 同上（`BATCHB_GATE=warn` 时四层全跑完，exit 0） |
| L3 干净检出闸 | 6.8 s（提取 1 s + 单测 5.4 s） | 复用主 target，不重复编译 |
| 干净树 lib 单测 | **4032 passed / 0 failed** | `git archive HEAD` 提取，CWD 指向干净树 |

## `--full` 在 HEAD 上是红的（如实报告，不是本次引入）

`01_check_full.out`：`--full` 停在 **L2 批次B 闸②**（`scope_gate.py`）：

- 5 个生产文件未接 `scope::` 也未写 `SCOPE-EXEMPT`：
  `erp/payable_payment/tests/payable_linkage_db_tests.rs`、`flow/subprocess.rs`、
  `monitor/model.rs`、`shared/user_name.rs`、`system/setup/model.rs`
- 17 个测试面文件未登记进 `SKIP_MANIFEST`

这是**存量债**（ARCH-9 §0 D-6「已知红台账」），不是本卡引入的：本卡起手时它就已经红。
正因为如此，`--fast` **不含 L2** —— 把存量债挂到 push 前，钩子当天就会被 `SKIP_PREPUSH` 绕过。
`BATCHB_GATE=warn` 下四层全部跑通（`02_check_full_batchb_warn.out`，exit 0），说明 L3 与 L2 其余闸都健康。

## 本次顺带修掉的两个「门禁名存实亡」

1. **`.git/hooks/pre-commit` 里的 rustfmt 是死代码**：2026-11-09 有人把 rustfmt 检查写进
   `.git/hooks/pre-commit`，同一天 `core.hooksPath` 切到 `.githooks` → `.git/hooks/` 从此不执行。
   「提交前自动跑 fmt」只在文档里存在。已把该检查迁进 `.githooks/pre-commit`（闸 3），
   并删掉本地死文件。
2. **C7 G1 判据误伤测试代码**：v1 是纯 grep（只排注释），在本卡起手 HEAD 上红 2 处，
   两条都是 `contract_handler.rs` 的 `#[cfg(test)]` 里 `SELECT … FROM sys_change_log` 断言
   （测「改一次只写一行」）。已改为 `scripts/lint/sys_change_log_gate.py`：只判生产面
   （测试文件跳过 + `#[cfg(test)]` 项花括号配对跳过，与 `scope_gate`/`inventory_write_gate` 同口径），
   生产面仍 0 容忍，并新增 9 条判据自检语料。

## 复现命令

```bash
bash scripts/git/install_hooks.sh --check            # 钩子就绪？
bash scripts/check.sh --dry-run                      # 看会跑什么
bash scripts/check.sh --fast                         # L0+L1（pre-push 同款）
bash scripts/check.sh --full                         # 四层（发版前）
bash scripts/verify/clean_checkout_gate.sh --self-test   # L3 判据自检
bash scripts/lint/sys_change_log_gate.py --self-test     # C7 G1 判据自检
```
