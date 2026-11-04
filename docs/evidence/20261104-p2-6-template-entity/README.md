# P2-6 证据：流程「模板库」有没有独立模板实体

配套决策文档：`docs/architecture/P2-6-模板实体-决策.md`（结论：选 A，合并概念，不建表）。

## 文件

| 文件 | 探针 | 目标 | 说明 |
|---|---|---|---|
| `probe-architect-run1.txt` / `-run2.txt` | `scripts/verify/p2_6_template_probe.py` | HTTP `:8023` + `cloudpivot_manual` | 35 条断言（H1–H9 用户可见行为 + D1–D12 结构事实）。**两轮逐字节一致**（`diff` 为空）。`H5` 是唯一 FAIL（`expect=0` / `actual=2` = 模板库里混进 2 条非标准流程），它就是本卡要治的病，不是探针坏了。 |
| `identity-manual-run1.txt` / `-run2.txt` | `scripts/verify/p2_6_template_identity_probe.sh` | 纯 SQL，`cloudpivot_manual` | 12 条断言（无模板表、模板只是 flag、两个菜单一个表、错标 2 行）。两轮一致。 |
| `identity-audit-db.txt` | 同上 | 另一套库（`cloudpivot_audit`） | 交叉印证：同一结构在第二套库上结论相同（11 def / 9 标准 / 2 错标），说明不是某套库的偶发状态。 |

## 复跑

```bash
# HTTP + DB（cloudpivot_manual 的 admin 口令是 Manual@123456，不是默认 Admin@123456）
TEST_PASS='Manual@123456' python3 scripts/verify/p2_6_template_probe.py

# 纯 SQL（显式指定库，否则默认读 /root/.cloudpivot/snapshot-audit.env）
NIK_DATABASE_NAME=cloudpivot_manual NIK_DATABASE_PASSWORD=alun \
  bash scripts/verify/p2_6_template_identity_probe.sh
```

两个脚本都**只读**（只 SELECT / 只打接口 GET），可随时重跑。探针的期望值（`expect`）故意写成"干净状态"，所以现在跑必有一条 `H5 FAIL`：它就是待修项。

## 注意

- `probe-architect-run*.txt` 的 `D5`（审批实例）= 0 是 `cloudpivot_manual` 这套**手工取证库**的现状，不代表生产。
- 探针早期版本按 `items` 取分页字段，`DefPage` 实际字段名是 `list`，会得出「两个页面不是同一批行」的假结论；现版本已修（见 `p2_6_template_probe.py:107` 注释）。
