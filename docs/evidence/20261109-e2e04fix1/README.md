# E2E04-FIX-1 证据：手工凭证支持辅助核算（`aux_*` 不再恒 NULL）

- 卡号：`t_51944d63`
- 日期：2026-11-09
- 代码 commit：`bf3f7c8f`（脚本修订：`171fa241` / `c4b8f19f`，只动 `scripts/verify/e2e04fix1_manual_aux.py`）
- 145 部署：`cloudpivot-test` = `bf3f7c8f` 构建的二进制（mtime `2026-11-09 17:57:44`），`/health` = 200；
  前端 `dist` 已由同一提交构建并切换（旧 `dist` → `dist-prev-175907`，nginx :80 入口 = `dist/index.html` 入口
  = `assets/index-BQIwhiY5.js`，凭证页 chunk `assets/index-BFLsUIO0.js` 含 `voucher-create-aux` /
  `aux_dimensions` / `辅助核算，请选择`）

## 一、问题与修法（一句话）

`ManualEntryReq` 只有科目 / 借贷 / 摘要，`manual_in_tx` 拼 `EntryLine` 时把四个 `aux_*` 写死成 `None`
→ 手工凭证分录的辅助列恒 NULL，而 5601 / 5602.x / 5603 / 5711 / 2241.01 在
`biz_account.aux_dimensions` 里声明了 `DEPT` → `GET /ledger/aux-balance?aux_type=DEPT` 永远 0 行
（E2E-04 实测未挂维度 113 笔），部门费用只能靠自动凭证。

修法：DTO 增加四个可空辅助字段（`aux_dept_id` / `aux_supplier_id` / `aux_material_id` /
`aux_warehouse_id`，与 `biz_voucher_entry` 四列、`biz_account.aux_dimensions` 四码一一对应）；
提交前按科目声明校验必填（只卡新增/修改，不碰存量）；前端分录行按所选科目**动态**渲染维度下拉。
口径写进 `docs/ERP-辅助核算口径.md` §2 / §2.1。

## 二、145 真机验收（`scripts/verify/e2e04fix1_manual_aux.py`）

```
python3 scripts/verify/e2e04fix1_manual_aux.py --base http://127.0.0.1:8033/api
PASS=29 FAIL=0
```

完整输出见 `e2e04fix1_145_run.log`。关键行：

| 验收 | 实测 |
|---|---|
| ① 5602.03 不选部门 → 400 | `HTTP 400 … entries[0] 科目 5602.03（管理费用-办公费）要求部门辅助核算，请选择部门（aux_dept_id）`，且本前缀凭证 0 张（不落半张草稿） |
| ② 选部门 → 保存成功、分录有值 | 回执 `status=DRAFT`、`aux_dept_id=01a1b5f3-474c-74cb-b7bd-f0c8ecc260eb`；psql 直查 `biz_voucher_entry.aux_dept_id` 同值 |
| ③ 查辅助账能看到 | 审核 + 记账后 `GET /ledger/aux-balance?aux_type=DEPT&aux_id=…&period_from=2026-11` → `rows=[('5602.03','100.0000'), …]`，`aux_name=演练设备部`（按 `sys_dept` 取名，不是 UUID） |
| ④ 1002 不要求维度 | 建单 200，1002 分录四个 `aux_*` 全 `None` |
| ⑤ 存量不阻断 | 库里既有「要求 DEPT 而分录 `aux_dept_id` 为 NULL」的凭证详情照常 200，NULL 仍是 NULL（没被回填） |
| ⑥ 非法 aux id | `HTTP 400 entries[0].aux_dept_id 不是合法的 UUID：not-a-uuid` |
| ⑦ 收尾 | 自建凭证零残留；`erp.separation_of_duties.approve` 恢复 `true`（审核要过 SoD，脚本临时关闭并在 `finally` 恢复） |

> 说明：辅助账只算 `POSTED`，所以 ③ 必须走「审核 → 记账」两步；SoD 开关的临时关闭是
> 验收脚本行为（走真实 `PUT /support/config/{id}`，会留审计日志），收尾已恢复原值。

## 三、单测（99，真库「模块 × 进程 × 库」）

- `MODULES="e2e04aux=business::erp::voucher_write" RUNS=2 bash scripts/verify/run_lib_tests_by_module.sh`
  → 两轮均 `32 passed; 0 failed`（原 26 + 本卡新增 6：2 纯逻辑 + 4 真库）。
- 新增用例覆盖：维度解析（大小写/空白/重复/未知维度忽略）、缺维度 400 文案、
  带部门落库 + 审核记账后辅助账命中、无维度科目不需要 aux、存量 NULL 不被拦、非法 UUID 400。
- 全量分模块套件 `RUNS=1 MODULES="changelog flow_p0fix flow erp"`：见 `full_libtest_run.log`。
  - `changelog` 52/52、`flow` 547/547 全绿；
  - `erp` 段 **2022 passed / 2 failed**，两条失败**都不在本卡改动范围**（`git show --stat bf3f7c8f`
    里没有这两个文件）：
    - `contract_handler::tests::manual_sign_from_pending_keeps_approval_instance`
      —— 夹具自己 `Value::from(&Uuid)` 绑 `?::uuid` 报 `cannot bind parameter to column type uuid`
      （`contract_handler.rs:2235`，最近一次改动是 `bdd93c7c` E2E07-FIX-4），单跑必红；
    - `stocktake_tests::lnk04_excluded_inventory_actions_do_not_double_write`
      —— 单跑**通过**（本次单独复跑 ok），全量跑时因同库串扰变红（`0dad364d` E2E07-FIX-2 的联动夹具）。
    原始断言见 `erp_module_failures.txt`；
  - `flow_p0fix` 的 9 条用例本身全绿，但脚本的**泄漏校验**报红（P0FIX 库与模块库的
    `p0fix_*` 夹具都是 0 行）——该检查属 `business::flow::internal_control`，本卡未触碰 flow。

## 四、已知边界

- 存量历史凭证**不回填**（凭证是审计证据；`docs/ERP-辅助核算口径.md` §6 同一口径）。
- 辅助核算维度只有四个（部门 / 供应商 / 物料 / 仓库），系统没有「项目 / 人员」维度；
  卡面举例的 `aux_project_id` / `aux_person_id` 未实现（无对应列、无取数来源）。
- 不要求某维度的科目传了 aux 也不报错（多挂维度不会算错账）；前端只提交该科目真要的维度。
