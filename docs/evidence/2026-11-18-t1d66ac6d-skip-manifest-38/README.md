# t_1d66ac6d 证据：38 个测试面文件补登记 `scope_gate.py` SKIP_MANIFEST

- 卡：`t_1d66ac6d`【测试卫生·门禁】check.sh 全量在闸② 红
- 基线：HEAD `fb16fcd5`（= 上一张卡 `t_23aad8fd` 的提交）
- 结论：**闸②（本卡主题）已绿** —— 38 条逐条核实全是纯 `#[cfg(test)]` 测试面、全部登记，
  `check.sh` 全量档里 `批次B 闸② = 0 处 / 批次B 门禁全部通过`。
  **但全量档整体仍 rc=1**，红在 **L3 干净检出闸的 2 条单测**，与本卡无关（见 §4，已另立卡）。

## 1. 改动面（一个文件，只增不删）

`git diff --stat scripts/lint/scope_gate.py` → **63 insertions(+), 0 deletions(-)**：

| 变化 | 行数 | 说明 |
|:--|:--:|:--|
| `SKIP_MANIFEST = frozenset(` 之前的来由注释 | 25 | 记成因、分组计数（17/11/3/2/1×4/1）、逐条复核结论、`leave_flow_seed_guard.rs` 为什么登记而不是改名 |
| 三引号块里新增条目 | 38 | 每条按字典序插到既有条目之间（既有顺序一行未动） |

判据代码**一个字节没改**（口径 v2 的解析 / 断言 / 自测逻辑全在 diff 之外）。
完整 diff：`04_git_diff_scope_gate.patch`。

## 2. 38 条逐条复核（先核实、后登记）

`01_mount_declarations_38.txt` 是逐条复核的原始输出：对每条文件都去全树找挂载声明，
打印 `声明文件:行` + 前 4 行上下文。结论：

- **38/38 都有挂载，且都是纯 `#[cfg(test)]`**（`#[cfg(test)] mod x;`
  或 `#[cfg(test)] #[path = "x.rs"] mod x;` 两种写法）；
- 异常扫描两条 grep 均 **0 命中**：
  - `NO MOUNT` / `exists=False` → 0（没有一条是「找不到声明」）；
  - `cfg(any` / `cfg(not` / `feature =` → 0（**没有一条**是「可能进生产」的
    `#[cfg(any(test, feature = …))]` 形态，因此不需要按口径 v2 照判、也不需要补 `scope::`）。

分组计数（脚本算的，不是手抄）：purchase 17 / sales 11 / stock_policy 3 / erp 顶层 2 /
{contract, cost, inventory_aging, replenish} 各 1 / hr 1 = **38**。

### 特殊的一条：`business/hr/leave_flow_seed_guard.rs`

名字不带 `_tests`，但确实是测试面：

- `business/hr/mod.rs:247` → `#[cfg(test)] pub(crate) mod leave_flow_seed_guard;`
- 模块本体是 `hr_leave` 正式种子（`V0393`）的「借出 → 归还」守卫（抓 `jsonb` / 删链 /
  原样写回 / 逐项对账），`grep` 全树只有 `leave_bridge_db_tests.rs` 与
  `leave_handler_db_tests.rs` 引用它 → 非测试构建里根本不存在，编不出生产代码。
- **选「登记」而不是「改名成 `*_tests.rs`」**：它是**夹具支撑模块**不是用例套件，
  改名会让后来人以为那里面有 `#[test]` 用例。

## 3. 闸② 的证据链（真跑）

| # | 证据 | 结果 |
|:--|:--|:--|
| ① | `00_gate_before_38fail.out`（改前单跑闸②） | **rc=1，38 条 FAIL**，逐条点名 + 给可粘贴的行 |
| ② | `02_gate_after_green.out`（改后单跑闸②） | **rc=0**：app 根 扫描生产面 685 / 跳过测试面 **209**，`跳过清单与 SKIP_MANIFEST 逐条一致（209 条）`；kernel 根 36/2 一致 |
| ③ | `03_teeth_remove_one_entry.out`（反证：从**副本**删掉一条） | **rc=1**，精确点名 `…/purchase/framework_agreement_db_tests.rs` —— 断言有牙，登记条目是承重的（副本是 scratch 里的 `gate_teeth.py`，仓库文件未动） |
| ④ | `06_selftest.out`（`--self-test` 负例自测） | **13 组全 PASS**（含 ⑩ 幽灵 `#[path]` 藏生产文件、⑬ 搬迁未同步） |
| ⑤ | `05_check_sh_fast.out`（`check.sh --fast` = L0+L1） | **rc=0**「全部检查通过（L0 L1）」 |
| ⑥ | `08_check_sh_full_L3red.out`（`check.sh` 全量档） | L2 里 `闸①=0 处 闸②=0 处` + `批次B 门禁：全部通过` ← **本卡目标达成**；整档 rc=1 红在 L3（见 §4） |

## 4. 全量档逐层状态（为什么整档还是 rc=1）

| 层 | 结果 | 依据 |
|:--|:--:|:--|
| L0 静态闸 | ✓ | 全部 PASS（含 path_refs 1049 脚本 0 失效路径、迁移号唯一、任务链、JUnit 自检…） |
| L1 编译与类型 | ✓ | `cargo fmt --check` / `clippy --all-targets -D warnings` / `cargo check` / `cargo test -p cloudpivot-doc` / `pnpm typecheck` ×2 |
| L2 连库与存量 | ✓ | **闸② = 0 处**、闸① = 0 处、`批次B 门禁：全部通过`；闸③–⑦ 因「无库口令」按口径跳过（本机无 `/etc/cloudpivot/cloudpivot.env`） |
| L3 干净检出 | **✗** | 2 条单测红，**与本卡无关**（见下） |
| L4 空库首启 | ✓（单独跑） | L3 先红截断了 L4，另跑 `check.sh --fresh-db` 绿：真库（145）建 `cloudpivot_freshgate_*` → 迁移 380/380、max version 460、0 失败迁移、admin↔SUPER_ADMIN、`/health` 200 跑满 5s，验完自删。见 `07_check_sh_L4_only.out` |

### L3 的 2 条红：都是**存量红**，与本卡无因果关系

证明与本卡无关：L3 用 `git archive HEAD`（`fb16fcd5`）提干净树，**本卡改动没进 HEAD**；
且本卡改的是 Python lint 常量，编译不进 Rust 单测二进制。

1. `business::permission::scope_model::tests::d8c_wiring_anchors_exist_in_source`
   —— **CWD 口径不匹配**（L3 的 harness 问题，不是判据问题）：
   用例以 `Path::new("src")` 相对 CWD 找源码（`scope_model.rs:1295`），
   而 L3 是 `cd $CLEAN_TREE/backend && $BIN`（workspace 根，那里没有 `src/`）。
   实测（同一二进制）：
   - `10_L3_two_reds_cwd_crate_root.out`：CWD = `backend/app` → 该用例 **ok**（同时暴露第 2 条红）；
   - `11_L3_d8c_cwd_workspace_root.out`：CWD = `backend` → 该用例 **FAILED**
     「找不到 src/ 下的 rs 文件（单测须以 crate 根为 CWD）」。
   结论：改法二选一 —— 用例改用 `env!("CARGO_MANIFEST_DIR")`（推荐，最省），
   或 L3 在 `backend/app` 下再跑一遍。已另立卡。

2. `kernel_axis_tests::contract_aliases_match_handler_permissions`
   —— **真·源码不一致**：`M4-19`（提交 `b1772279`）给
   `contract_handler::PERMISSIONS` 加了第 6 个码 `business:erp:contract:execution`
   （`contract_execution_dto::EXEC_PERMISSION`），但内核
   `permission_alias::LEGACY_ALIASES` 仍是 5 行，用例断言
   `规范码集合 == PERMISSIONS` 且 `LEGACY_ALIASES.len() == 5` → 必红。
   **不能靠加一条假别名糊过去**：该表语义是「已从 `erp:contract:*` 切换过来的码」，
   `execution` 是 P2-4 之后新增的码，从来不存在 `erp:contract:execution` 这个旧码。
   正确改法要动**判据**（`==` → 「别名规范码 ⊆ PERMISSIONS」+「PERMISSIONS 里多出来的
   只许是新增码白名单」），属于门禁口径调整，需按「判据只加不减」约定评估。已另立卡。

## 5. 顺带发现并立的 3 张后续卡

| 卡 | 内容 |
|:--|:--|
| `t_71d2aa63` | L3 红①：`d8c_wiring_anchors_exist_in_source` 用 CWD 相对路径找 `src/`（L3 从 `backend/` 跑 → 必红）；实测同二进制换 CWD 即绿，见 §4 第 1 条 |
| `t_a5eca17f` | L3 红②：合同域第 6 个码 `business:erp:contract:execution`（`M4-19` / `b1772279`）未纳入双码守门人 → `kernel_axis_tests` 必红；**不许加假别名糊过去**，要改判据口径 |
| `t_f484849d` | `task_chain_valid.py --fix` 在本卡这个「2 张游离清债类卡」形态下**不收敛且把主链打散**（干跑 200 处调整、真跑同样 200 轮、终局主链塌成 1 张）。证据见 `docs/evidence/2026-11-18-chainfix-probe/` |

第 3 张是本卡施工中踩出来的：按团队约定建完卡要跑 `--fix` 串链，干跑输出明显不对劲（同一张卡
重复 198 次），于是**没敢真跑**，改用手工落边（`t_1d66ac6d → t_71d2aa63 → t_a5eca17f →
t_f484849d → t_18b222b9`，落完 `task_chain_valid.py` 检查模式 `✓ 结构完整`），随后在**副本库**上
真跑 `--fix` 复现了「链被打散」，证据落盘后才立卡。

## 6. 文件清单

| 文件 | 内容 |
|:--|:--|
| `00_gate_before_38fail.out` | 改前闸②：rc=1，38 条 FAIL |
| `01_mount_declarations_38.txt` | 38 条逐条复核（声明位置 + `#[cfg(test)]` 上下文） |
| `02_gate_after_green.out` | 改后闸②：rc=0，209 条逐条一致 |
| `03_teeth_remove_one_entry.out` | 反证：删掉一条 → 精确点名报红 |
| `04_git_diff_scope_gate.patch` | 完整 diff（+63 / −0） |
| `05_check_sh_fast.out` | `check.sh --fast` rc=0 |
| `06_selftest.out` | `--self-test` 13 组全 PASS |
| `07_check_sh_L4_only.out` | `check.sh --fresh-db`（L4）真库绿 |
| `08_check_sh_full_L3red.out` | 全量档：L2 闸② 绿、L3 红（2 条存量红） |
| `10_L3_two_reds_cwd_crate_root.out` | CWD=backend/app：d8c ok / alias 红 |
| `11_L3_d8c_cwd_workspace_root.out` | CWD=backend：d8c 红「找不到 src/」 |
