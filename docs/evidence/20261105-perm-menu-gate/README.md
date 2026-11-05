# 死权限码常驻闸 · 验收证据（t_33377103，2026-11-05）

> **一句话**：`backend/src` 里 `permission = "…"` 用到的码，在 `sys_menu.auth_code`
> 里必须都有承载行 —— 没有就意味着「除超管通配外谁都授不到」，功能上线即死锁。
> 本卡把这条人工比对固化成常驻脚本 + `scripts/check.sh` 闸⑥。

## 0. 为什么（来源是现场，不是猜）

`t_34cee9ea` 补业务角色权限码时人工比对发现：

```
handler 在用 274 条 / sys_menu 282 条
差集（代码在用、菜单没有）：
  business:erp:voucher:audit      voucher_write.rs:1589
  business:erp:voucher:un-audit   voucher_write.rs:1614
  business:erp:voucher:post       voucher_write.rs:1638
```

后果具体：**财务经理有 `voucher:list` 却永远审不了、过不了凭证** —— 这 3 个码在
`sys_menu` 里没有挂载行，`sys_role_menu` 授不出去。当时已补齐（迁移
`V0250__erp_voucher_three_state` 种 3 条 `menu_type=3` 行；生产由
`scripts/datafix/bizperm/20_grant.up.sql` 带 `IF NOT EXISTS` 幂等兜底）。
**人工比对只做一次，不做成常驻检查，下一个 `permission = "…"` 还会再长出来。**

## 1. 交付物

| 文件 | 作用 |
|------|------|
| `scripts/verify/perm_menu_gate.py` | 常驻闸本体：扫源码注解码 × 比菜单 `auth_code`（只读） |
| `scripts/check.sh`（闸⑥） | 接进既有检查链（`PERM_MENU_GATE=block/warn/off`，无库环境跳过） |
| 本目录 | 全部原始输出 |

判据两档，只有第一档判红：

```
missing_in_menu = 注解码 − 菜单 auth_code   ← 必须 0，否则 exit 1 并点名到文件:行
unused_in_code  = 菜单 auth_code − 注解码   ← 只报告不判红（行级校验 / 纯前端权限点合法）
```

白名单必须带理由：源码 `// PERM-EXEMPT: <理由>`（注解上方 8 行内）或脚本内
`EXEMPT = {码: "理由"}`；**空理由/太短/裸 set → 直接判红**。

## 2. 验收对照

| 卡面验收 | 结论 | 证据 |
|---------|------|------|
| ① 在生产库上跑出 `missing_in_menu = 0`，输出贴进证据目录 | ✅ | `01_gate_run.txt`（A 段，隔离库 = 迁移全量）；`03_prod_144_run.txt`（A 段，**144 生产库直连**） |
| ② 故意注释掉一条 `sys_menu` 行时 exit 1 并点名该码 | ✅ | `01_gate_run.txt` B 段：软删 `business:erp:voucher:audit` → `missing_in_menu = 1`、`EXIT=1`、点名到 `voucher_write.rs:1589`；C 段恢复后回绿。`03_prod_144_run.txt` B 段：从生产快照挖掉 `business:erp:voucher:list` → 红并点名。`06_gate_func_three_states.txt`：把 `check.sh` 里的闸⑥ 函数原样抽出跑「绿 → 红 → 绿」三态 |
| ③ `scripts/check.sh` 里能看到这条闸，且本地全绿 | ✅（闸⑥ 自身全绿，见 §4 关于同仓其他卡的既有红点） | `02d_check_sh_final.txt`（**提交版代码**的全链路：fmt/clippy/check/typecheck → 闸①–⑤ → 闸⑥ PASS，`EXIT=0`）；`02c_check_sh_warn_gates.txt`（同口径的上一轮）。**闸有牙**：`05_check_sh_blocks.txt` —— 软删一行后跑完整 `check.sh` → 闸⑥ 拦住、`EXIT=1`、点名 `business:erp:voucher:audit` |
| ④ 提交 + 推双远端并回读 | ✅ | 提交 sha 见卡面 completion（`git ls-remote` 双远端回读同一 sha） |
| 附加：白名单「必须带理由」 | ✅ | `04_whitelist_e2e.txt`：F1 裸注解 → 红；F2 加理由 → 放行且进明细；F3 空理由 → 重新判红 |
| 附加：闸门自检（防判据被改坏） | ✅ | `01_gate_run.txt` E 段 / `02d_check_sh_final.txt`：19/19 通过 |

## 3. 基线（本卡实测，已写进脚本头注释）

生产库 `cloudpivot@172.18.10.144`，2026-11-05 17:14 快照：

| 项 | 值 |
|----|----|
| `sys_menu` 活跃行 / 去重 `auth_code` | 328 行 / **285 个** |
| 主分支（`7be33ba`）`backend/src` 注解码 | **273 个**（486 处，95 个码被多处复用） |
| **`missing_in_menu`** | **0** ✅（本次补齐的 3 条凭证码已在菜单里） |
| `unused_in_code` | **12**（只报告） |

`unused_in_code` 逐条（脚本输出原样）：

```
business:attachment:delete         行级/常量引用  business/attachment/dto.rs:31
business:erp:skip-approval         行级/常量引用  business/erp/approval_gate.rs:76
business:flow:admin                行级/常量引用  business/flow/handler.rs:1594
business:project:delete            行级/常量引用  business/project/handler.rs:57
business:project:update            行级/常量引用  business/project/handler.rs:55
business:task:attachment:delete    行级/常量引用  business/task/dto.rs:1407
business:task:comment:delete       行级/常量引用  business/task/dto.rs:1404
business:task:dependency           行级/常量引用  business/task/dto.rs:1093
business:task:member               行级/常量引用  business/task/dto.rs:107
business:task:worklog              行级/常量引用  business/task/dto.rs:1410
system:field-mask:plain            行级/常量引用  business/masking/registry.rs:47
system:preference:view             源码零引用（纯前端权限点，FIX-06 C 类台账在案）
```

**与卡面的差 1 条**：卡面写 `unused_in_code = 13`，实测 **12**。卡面的 13 来自更早的
快照（菜单 282 / 代码 274，其中 1 条是单测夹具 `x:y:z`）；本次口径剥了注释、排除了
测试面与 `cfg(test)` 夹具，逐条复核后是 12。**不按卡面数字凑**，以实测为准。

## 4. 同仓其他卡的既有红点（不是本卡引入，但会挡住全链路）

`scripts/check.sh` 默认口径在本工作区**跑不到闸⑥**，原因全在别的卡的在途改动上
（本卡只改了 `scripts/check.sh` + 新增 `scripts/verify/perm_menu_gate.py`，未碰任何 Rust）：

1. 前端 fmt/clippy/check/typecheck → **全绿**；
2. 批次B 闸②：`scope_gate.py` 的 `SKIP_MANIFEST` 里没有登记 6 个新测试文件
   （`expense_model_tests.rs` / `stockout_fifo_tests.rs` / `task_handler_*_tests.rs` /
   `cache_tests.rs`）—— 别的卡新加的测试文件，`check.sh EXIT=1`（`02_check_sh.txt`）；
3. `BATCHB_GATE=warn` 放过后，闸④ 报 2 个词表域外值 `EXPENSE_PAYMENT` / `EXPENSE_REPORT`
   （FIN-3 费用模块的 `biz_type`，词表 `doc_kind.rs` 未同步）→ 仍 `EXIT=1`
   （`02b_check_sh_batchb_warn.txt`）；
4. 再加 `BIZ_VOCAB_GATE=warn` 才走到闸⑥ —— 闸⑥ **PASS**（`02c_check_sh_warn_gates.txt`）。

> 这两处红点均有独立归属（`expense_*` / `stockout_fifo_*` / FIN-3 词表），
> 本卡的 `git status --porcelain` 里只有 `scripts/check.sh`、`scripts/verify/perm_menu_gate.py`、
> `docs/evidence/20261105-perm-menu-gate/` 三项是自己的。

## 5. 环境与复现

```bash
# 1) 判据自检（不连库）
python3 scripts/verify/perm_menu_gate.py --self-test          # 19/19 通过

# 2) 连库跑（本机 99 的隔离库；144 生产库用 /etc/cloudpivot/cloudpivot.env）
NIK_DATABASE_NAME=cloudpivot_permgate NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  python3 scripts/verify/perm_menu_gate.py

# 3) 离线比对生产快照（无库环境 / CI）
python3 scripts/verify/perm_menu_gate.py --menu-codes-file <生产快照.txt>

# 4) 全链路（工作区有别的卡的既有红点时按开关放过它们，只验闸⑥）
BATCHB_GATE=warn BIZ_VOCAB_GATE=warn bash scripts/check.sh
```

隔离库 `cloudpivot_permgate` 由「`createdb -O alun` + 起一次 `target/debug/cloudpivot`
让 193 个迁移自动应用」建出来：`sys_menu` 328 行 / 285 个码 / 3 条凭证按钮码 ——
**与生产一致**，说明 `V0250` 已经把 3 条菜单行种进迁移，新装库不会再缺。
（验完可删：`su postgres -c 'dropdb cloudpivot_permgate'`。）

本目录里的三份复算脚本可原样重跑（`REPO` / `DB` / `OUT` 均可用环境变量覆盖）：

| 脚本 | 产出 |
|------|------|
| `repro_setup_db.sh` | 建隔离库并让迁移跑完（含 `sys_menu` 三项事实核对） |
| `repro_gen_01_gate_run.sh` | 生成 `01_gate_run.txt`（连库跑 / 软删反向 / 恢复 / 生产快照 / 自检） |
| `repro_gen_04_whitelist.sh` | 生成 `04_whitelist_e2e.txt`（在 src 临时副本上验白名单三态） |

## 6. 顺带发现（不影响验收，供排期参考）

生产树 `7d96692` 的注解码是 **270 个**，比主分支 `7be33ba` 少 3 个 —— 少的正是
`business:erp:voucher:{audit,post,un-audit}`：**菜单行已随 V0250 上线，但三个 handler
的注解还没随二进制上生产**。所以当前线上「授了也用不上」（按钮没接口拦），
下次后端发版后自动生效。两条分支上闸都是 `missing_in_menu = 0`，无需动作。
