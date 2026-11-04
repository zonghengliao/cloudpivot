# P2-6b 证据：`templates()` 收口为「标准流程」列表

> 卡：`t_850c7eae`【P2-6b】接口收口：`templates()` 加 is_system 过滤 + scope 参数
> 决策：`docs/architecture/P2-6-模板实体-决策.md`（A：合并概念，不建表；「模板」= `biz_flow_def.is_system=TRUE` 这个角色）
> 时间：2026-11-05 01:0x–01:2x CST｜机器：`ubuntu-svr`（172.18.10.99，工作仓 `/opt/cloudpivot`）
> 被测实例：`127.0.0.1:8023`（pid 2976259，`NIK_DATABASE_NAME=cloudpivot_manual`，本次用新二进制重启）

## 0. 一句话结论

`GET /business/flow/def/templates` 默认只回 **标准流程**（`is_system = TRUE`）；
`?scope=all`（legacy `?include_mine=1`）才带客户副本；**不按 `status` 过滤**（停用的标准流程仍在）。
端点名保留（legacy 别名），DTO 字段名一个没改。

## 1. 改了什么（4 处 / 3 个文件 + 测试）

| 文件 | 改动 |
|---|---|
| `backend/src/business/flow/model.rs` | 新增 `TemplateScope { Standard, All }`；`templates(scope)` 在 `Standard` 口径里加 `AND d.is_system`，`All` 不加；**两种口径都不带 `status` 条件** |
| `backend/src/business/flow/handler.rs` | `templates(Query<dto::TemplateQuery>)`：`scope`（`standard` 默认 / `all`）优先，legacy `include_mine` 兜底；非法 `scope` → `400 + 65005`；补 `legacy 别名` 注释；`duplicate()` 仅改注释 |
| `backend/src/business/flow/dto.rs` | 新增 `TemplateQuery`；`TemplateItemRes/GroupRes/Res` **只改注释**，字段名一个不动 |
| `backend/tests/pflow5.rs` | 3 条新用例（默认口径 / legacy 等价 / 停用仍在）+ 原 4 条读列表用例改走 `scope=all` |
| `scripts/verify/p2_6_template_probe.py` | 断言口径跟随收口：H4/H5 期望 0、新增 H10–H14（`scope=all`、total=库里标准流程数）、H8 由「两页条数相等」改为「子集视图」 |
| `scripts/verify/p2_6_template_identity_probe.sh` | P4a 从「WHERE 里没有 is_system（应=1）」翻成「标准口径带 is_system 过滤（应=1）」；P2b 改为不变量「错标行数=0」 |

## 2. 逐条验收（真实输出，见同目录文件）

### 2.1 造副本 → 默认口径 0 条非系统、`include_mine=1` 能回出副本

`acceptance-run1.txt`（`python3 t850_acceptance.py`，19 条断言 FAIL=0）：

```
-- 基线：未软删流程 13 条（is_system=TRUE 11 / FALSE 2）
D0     OK     duplicate HTTP 200 flow_code=stock_transfer_custom is_system=False status=DRAFT
D1     OK     DB 侧副本已入库：is_system=false 条数 expect=3 actual=3
  [默认/副本存在] total=11 条目=11 is_system=false=0 分组=7
D2     OK     默认口径 is_system=false 条目数 expect=0 actual=0（DB 里却有 1 条 → 过滤真的生效）
D3     OK     默认口径不含副本 stock_transfer_custom（actual 含=False）
D4     OK     默认口径 total=标准流程数 expect=11 actual=11
D5     OK     ?include_mine=1 回出副本 expect=True actual=True total=14
  [scope=all] total=14 条目=14 is_system=false=3 分组=7
       非标准条目：stock_transfer_custom status=DRAFT
       非标准条目：asset_disposal status=PUBLISHED
       非标准条目：budget_approve status=PUBLISHED
D6     OK     ?scope=all 回出副本 actual=True total=14
D7     OK     include_mine=1 与 scope=all 等价 expect=14 actual=14
D8     OK     scope=all total=基线+1 expect=14 actual=14
D9     OK     scope=standard 优先于 include_mine=1（副本出现=False）
D10    OK     非法 scope → HTTP 400 code=65005（期望 400 / 65005） msg=scope mine 非法（只允许 standard / all）
D11    OK     停用的标准流程仍出现在默认结果里（命中 1 条，status=DISABLED）
D12    OK     停用操作不影响 is_system 过滤
D13    OK     已还原 voucher_reverse 状态 expect=PUBLISHED actual=PUBLISHED
C1     OK     副本已硬删（残留行 expect=0 actual=0）
C2     OK     库回到基线：行数 13=13 / 标准 11=11
C3     OK     清理后默认口径 total=11（= 标准流程数 11）
```

> **关键点（卡面点名的那条）**：`D1` 证明此刻库里 `is_system=false` 有 3 行，
> 而 `D2` 证明接口默认口径返回 0 行 —— 过滤确实生效，不是「库里本来就没有」的假绿。

### 2.2 停用的标准流程仍在默认结果里

见上 `D11/D12/D13`：把 `voucher_reverse` 的 `status` 改 `DISABLED` → 默认口径仍命中 1 条且
`status=DISABLED` → 还原为 `PUBLISHED`（`D13` 复核）。

### 2.3 探针连跑两轮（H5 OK / diff 为空）

`probe-run1.txt` / `probe-run2.txt`（`TEST_PASS='Manual@123456' python3 scripts/verify/p2_6_template_probe.py`）：

```
H4     OK       expect=0          actual=0            其中 is_system=FALSE（默认口径下必须 0）
H5     OK       expect=0          actual=0            默认口径里混进了非标准流程的数量（期望 0 = 过滤生效…）
H12    OK       expect=11         actual=11           默认口径 total 必须 = 库里 is_system=TRUE 条数
H14    OK       expect=13         actual=13           scope=all 的 total 必须 = 库里未软删定义数
-- 断言 39 条，FAIL=0
```

`diff probe-run1.txt probe-run2.txt` → **空**（两轮逐字节一致，探针输出不含时间戳/pid 抖动）。

另：`probe-with-copy.txt` 是**副本存在时**跑的同一支探针（`t850_copy.py make` → 探针 → `drop`）：

```
make: HTTP 200 code=stock_transfer_custom is_system=False status=DRAFT
DB 非系统行数 = 3
H5     OK   expect=0  actual=0    ← 库里有 3 行非系统，接口 0 行
H12    OK   expect=11 actual=11
H14    OK   expect=14 actual=14
-- 断言 39 条，FAIL=0
```

### 2.4 静态核对（过滤真的写进 SQL 了）

`identity-human.txt` / `identity-run{1,2}.txt`（`bash scripts/verify/p2_6_template_identity_probe.sh`）：

```
P4a    OK       expect=1          actual=1          model::templates() 标准口径带 is_system 过滤（应 = 1；P2-6b 前这里是 0）
P2b    OK       expect=0          actual=0          错标行数是否为 0
P3a    OK       expect=0          actual=0          另存为出来的副本数（flow_code 以 _custom 结尾）
-- 结论：FAIL=0
```

两轮 `--json` 输出逐字节一致（`diff identity-run1.txt identity-run2.txt` → 空）：
`{"db":"cloudpivot_manual","def_total":13,"is_system_true":11,"is_system_false":2,"mislabeled":0,"custom":0,"versions":14,"instances":0,"fail":0}`

### 2.5 编译 / 检查 / 单测

- `cargo-check-clippy-fmt.txt`：`cargo check --all-targets` 0 警告、`cargo clippy --all-targets -- -D warnings` 0 警告、4 个文件 `rustfmt --edition 2024 --check` 全 clean。
- `cargo-test.txt`：
  - `cargo test --lib business::flow` → **350 passed / 0 failed**（原 346 + 新增 4 条 `template_scope_tests`）
  - `PFLOW5_DB_URL=…/cloudpivot_test_pflow5 cargo test --test pflow5 -- --test-threads=1` → **15 passed / 0 failed**
    （12 条原用例 + `templates_default_scope_only_returns_standard` / `templates_legacy_include_mine_equals_scope_all` / `templates_default_scope_keeps_disabled_standard`）

## 3. 环境说明（会影响复跑）

1. 被测实例 `8023` 由本卡用**新二进制**重启（原进程 01:10 起的老二进制）。启动时
   `cloudpivot_manual` 自动补跑了 `V0221`–`V0232`（该库此前停在 V0220），所以基线从
   11 条定义变成 13 条 —— 所有断言都按「库里真实数字」动态取，不写死。
2. `cloudpivot_test_pflow5` 是本次为跑集成测试建的隔离库（`pg_dump cloudpivot_manual`
   → `createdb`(postgres) → `pg_restore`），跑完保留供复跑；`cloudpivot_t850c7eae` 是准备阶段的
   同源副本，本轮未使用。
3. 清理：副本 `stock_transfer_custom` 已**硬删**（node/edge/version/def 全清，`C1`=0），
   `voucher_reverse` 的 `status` 已还原（`D13`），`cloudpivot_manual` 回基线（`C2`）。

## 4. 留下的问题（不在本卡范围，已建跟进卡）

`V0228`（`budget_approve`，预算审批）与 `V0231`（`asset_disposal`，资产处置审批）插入预置流程时
**没有显式写 `is_system = TRUE`**，吃默认 `FALSE`（两处 `INSERT INTO biz_flow_def (…)` 的列清单里都没有
`is_system`）。P2-6a 修的 2 条是「名字含『标准』」那一类，这两条名字里没有「标准」，所以既没被
V0220 覆盖、也不会被「名字含标准却非系统」这条不变量抓到。

后果：本卡收口后这两条**从标准流程列表里消失**（它们出现在 `?scope=all` 里，见 2.1 的
`scope=all` 明细）。流程定义页（`/def/page`）仍能看到它们，故不是数据丢失，但「模板库」页会少两张卡。
修复需要一条新迁移（`V0233`）+ 把「预置流程种子必须显式写 `is_system`」变成可执行断言，
属另一张卡（已在看板创建）。
