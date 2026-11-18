# M4-16 供应商评估「前端页面 = 评估录入 + 排名」· 证据包

- 卡：`t_005d6b65`（M4-16【M4·采购】供应商评估：前端页面）
- 上游：`t_7938e24a`（测试卫生·仓储）
- 日期：2026-11-18
- 结论：**页面 + 它需要的最小接口面一起交付**（5 条路由 / 2 个接口码 / 1 个页面显形码 /
  3 行菜单 V0457）。演练 39 条断言全绿；真库用例 4 条 + DTO 纯逻辑 14 条；前端 15 条单测；
  三处变异反证全部按预期转红。

## 一、为什么接口落在本卡（范围决定，不是夹带）

卡面写「本卡只做这一件事：评估录入 + 排名」。落到代码时发现：M4-12 只交表结构、M4-13 只交
打分与评级的数据层（两者都零路由、零权限码），链上也没有「供应商评估后端接口」卡 —— 页面离开
接口就是空壳（点「评估录入」必然 404）。

M4-13 的证据包 §四已经把这件事写死：

> **M4-16（前端页面）**：本卡**没有接口**……页面要的「列表 / 详情 / 打分 / 重评级 / 交期参考分」
> 需要一组 dto + handler + 路由 + 权限码 —— 建议照 M4-11→M4-11b 的做法**另开一张后端接口卡**
> （`t_f358da98` 即该先例），不要把权限码塞进前端卡里现造。

本卡照 M4-11b 的先例**同卡交付最小接口面**，并把范围钉死成「页面用得到的 5 条」：

| 方法 | 路径 | 权限码 | 页面哪块用 |
|---|---|---|---|
| GET  | `/business/erp/supplier-evaluation/page` | `…:list` | 台账分页（关键字 / 等级） |
| GET  | `/business/erp/supplier-evaluation/rank` | `…:list` | 本期排名（期间必填，按总分降序） |
| GET  | `/business/erp/supplier-evaluation/{id}` | `…:list` | 详情抽屉 |
| GET  | `/business/erp/supplier-evaluation/delivery-suggestion` | `…:save` | 打分表单「取交期参考分」 |
| POST | `/business/erp/supplier-evaluation` | `…:save` | 打分录入（同期间覆盖） |

**不做**：导出、批量重评级、删除评估单。理由写进 `supplier_evaluation_handler.rs` 头注释
（评估是打分事实，改分=重新打分；要作废一家供应商走 M4-14 准入/黑名单流程）。

判据一行没重写：阈值 / 算分 / 落库复用 M4-13 `grading`，读入口复用 M4-12 `model`，
本层只做「入参校验 + 出参整形」。

## 二、改动文件

```
frontend/src/views/business/erp/supplier-evaluation/index.vue          （新）台账 + 本期排名 + 打分抽屉 + 详情抽屉
frontend/src/views/business/erp/supplier-evaluation/supplier-evaluation-logic.ts （新）纯逻辑：定点预览总分 / 权重和 / 期间预设 / 表单校验
frontend/src/api/business/erp/supplier-evaluation.ts                   （新）5 个调用 + 全字段类型（Decimal 按字符串）
frontend/src/router/index.ts                                           （改）注册 /business/erp/supplier-evaluation + meta.permission
frontend/tests/unit/supplier-evaluation-logic.test.ts                  （新）15 条纯逻辑单测
backend/app/src/business/erp/purchase/supplier_evaluation_dto.rs       （新）入参校验 + 出参整形 + 14 条纯逻辑单测
backend/app/src/business/erp/purchase/supplier_evaluation_handler.rs   （新）5 条路由 + build_rank
backend/app/src/business/erp/purchase/supplier_evaluation_handler_db_tests.rs （新）4 条真库用例（含 zz_ 清场）
backend/app/src/business/erp/purchase/supplier_evaluation_model.rs     （改）+EVALUATION_TARGET / page_evaluations / evaluations_in_period
backend/app/src/business/erp/purchase/mod.rs                           （改）注册四个模块
backend/app/migrations/business/V0457__supplier_evaluation_page.{up,down}.sql （新）3 行菜单 + 采购/财务授权 + 自检
backend/app/src/system/menu/permission_audit_tests.rs                  （改）页面码登记 ROUTE_LEVEL_ONLY；框架协议页例外销号（见 §五）
scripts/verify/m416_supplier_evaluation_page_drill.sh                  （新）39 条断言的一键演练
scripts/verify/menu_invariants.sh + scripts/verify/README.md           （改）⑤ 菜单总数上限 560 → 580（实测 563，见 §五）
docs/dead-permissions-20261101.md                                      （改）新增 §14 供应商评估页的页面显形码
```

零新表、零新错误码（400 走既有 `PARAM_INVALID`、404 走 `RESOURCE_NOT_FOUND`）。

## 三、验证（全部真跑，原始输出在本目录）

| 验证项 | 结果 | 原始输出 |
|---|---|---|
| 一键演练 `scripts/verify/m416_supplier_evaluation_page_drill.sh` | **PASS=39 FAIL=0，rc=0** | `04_drill.txt` |
| 真库用例（覆盖语义 / 台账筛选 / 排名排序 / 清场） | **4 passed / 0 failed / 0 SKIP** | `04_drill.txt` §6 |
| DTO 纯逻辑（400 分支 + 出参整形） | **14 passed / 0 failed** | `04_drill.txt` §6 |
| 前端纯逻辑单测 `npx tsx --test` | **15 passed / 0 failed** | `03_unit_logic.txt` |
| `pnpm typecheck` | rc=0 | `01_typecheck.txt` |
| `pnpm build` | rc=0，页面已进产物（`sev-list-table` 命中 `dist/assets/index-*.js`） | `02_build.txt` |
| `cargo clippy -p cloudpivot --lib --tests -- -D warnings` | rc=0（Finished 2m55s，零告警） | `05_clippy_lib_tests.txt` |
| `rustfmt --check`（6 个改动 .rs） | 0 diff | `06_fmt.txt` |
| 权限审计 N14–N21 | **8 passed / 0 failed** | `04_drill.txt` §7 |
| n08 菜单路径 → 前端路由 | **1 passed**（3 条新菜单 path 全部能解析） | `04_drill.txt` §7 |
| `menu_invariants.sh` 8 条 | 全绿（⑤ 563 在上限 580 内） | `04_drill.txt` §7 |
| 迁移幂等 / 可回滚 | up 重跑 3 行不变；down 后菜单与授权 0 行且**表不删**；再 up 成功 | `04_drill.txt` §2 |
| 夹具残留 | `M416-` 评估 0 / 供应商 0 | `04_drill.txt` §8 |

### 三处变异反证（证明用例有牙）

| 变异 | 期望 & 实测 | 输出 |
|---|---|---|
| A 排名 SQL `ORDER BY total_score DESC` 改 `ASC` | 排名用例红在 `left: ["70.00","85.00","92.00"]` | `07_mutations.txt` |
| B 台账 SQL 忽略等级筛选 | 分页用例红在 `left: 3 / right: 1` | `07_mutations.txt` |
| C `build_rank` 名次从 0 起 | 排名用例红在 `left: [0,1,2]` | `07_mutations.txt` |

三处均已还原；还原后 `business::erp::purchase::supplier_evaluation` 全组 **71 passed / 0 failed**。

## 四、页面口径（写死在注释里，别再猜）

- **总分与等级都是后端的**：入参没有 `total_score`（库层生成列，写不进），等级由 M4-13 按
  `GRADE_THRESHOLDS`（A≥90 / B≥80 / C≥70 / D<70，含下界）写。页面只有「预览」（定点整数运算，
  中点远离零舍到 2 位，与 PG `round(numeric,2)` 同公式），且文案写明「保存后以后端为准」。
- **一份期间一个结论**：同供应商同期间再保存 = 覆盖（单号不变，`created=false`），所以按钮文案是
  「重新打分」不是「新建」。
- **权重必须精确 = 1**：页面 `weightsState` 与后端 `Weights::is_valid` 同一份判据，0.9999 也拦。
- **交期参考分只有交期有客观数据**：质量 / 价格 / 服务无数据源（M4-13 §四），
  `suggested_score === null` 时显示口径说明，**不显示 0 分**（0 分 = 全部逾期）。
- **权限**：页面显形码只给路由 `meta.permission`；`list` 管全部只读面，`save` 只管打分录入。
  财务经理只拿 `page + list`（自己打分自己复核 = 自审）。

## 五、两处附带修复（都是「不修就交不了货」的共享门禁）

1. **N18 台账过期（HEAD 上就红，不是本卡引入）**：`framework-agreement:page` 仍挂在
   `ROUTE_LEVEL_ONLY`，但 M4-11b 在 `framework_agreement_dto.rs` 用例里写了字面量负断言，
   而审计口径（§口径 3）把 `#[cfg(test)]` 里的引用也算「有承载」⇒ N18 判台账过期。
   实证：**只应用到 V0456 的库里跑 n18 即红**（本卡 V0457 无关）。修法：按 N18 的指示销号，
   并在台账位置写清「该码仍是纯前端显形码，完整记录见 dead-permissions §12」。
2. **菜单总数上限 560 → 580**：模板库升级路径（V404 → V457 全链）实测 **560 → 563**，
   本卡 3 行菜单正好破线。按 `menu_invariants.sh` 头注释的既定流程「先确认是真实功能增长，
   再同步改这里和 scripts/verify/README.md」调整，历史写在注释里。

## 六、边界（不是 bug，是决定）

- **没有跑 e2e 的 30-menu-alignment / 31-dead-buttons**：它们要一个**已部署**的实例，
  本卡不部署（部署由人审批）。等价证据已给出：n08（菜单 path ↔ 路由）真库绿 + 页面已进构建产物 +
  接口 5 条全有真库用例。上线后建议补跑 `frontend/e2e/30-menu-alignment.spec.ts`。
- **排名名次不并列**：排序键 `total_score DESC, evaluation_no`，「同分两家」拿相邻两个名次。
  要竞赛名次（1,1,3）是另一个业务口径，本卡没做也没猜。
- **M4-13 的演练脚本会因本卡转红**：`m413_..._drill.sh` §4 断言「菜单里没有供应商评估相关行
  （页面属 M4-16）」—— 那是对 M4-13 时点的断言，本卡落地后它必然红。属于预期，不是回归。
