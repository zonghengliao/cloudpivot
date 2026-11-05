# P1-FIX-5C（architect / t_21dc81c3）：联动触发点「已接线 / 未接线」清单 + 配置页防误配

**卡**：`t_21dc81c3`（architect）· 父卡 `t_53c23814`（P1-FIX-5B）· 同一 P1-FIX-5C 的实现侧卡 `t_befea97a`（backend-developer）
**一句话**：白名单 15 个触发点里**只有 3 个真的有人调联动引擎**（`STOCK_IN` / `STOCK_OUT` / `TASK`），
其余 12 个「能保存、能启用、永远不触发」。本卡的交付是**把这个事实变成可自动核对的事实**，
并让配置页**不可能静默保存**一条永远不会触发的规则。

- 设计与清单文档：`docs/architecture/P1-FIX-5C-联动触发点接线清单.md`（§3 逐条清单、§7 分批计划、§10 本目录的验收记录）
- 判据唯一真相：`backend/src/business/flow/linkage/spec.rs::TRIGGER_SPECS`（`wired` / `wiring` / `proofs`）
- 漂移守卫：`backend/src/business/flow/linkage/tests_wiring.rs`（正向 proofs + 反向全库扫描）

> 与实现卡的分工：实现卡负责 `wired` 字段/硬门/`STOCK_OUT` 接线（`ea4d2c5`，19 条端到端断言）；
> 本卡负责**设计决策、15 条清单文档、前端三道防线与独立验收**。证据目录刻意分开，互不覆盖。

---

## 1. 交付物

| 交付物 | 位置 | 说明 |
|--------|------|------|
| 清单 + 决策文档 | `docs/architecture/P1-FIX-5C-联动触发点接线清单.md` | 15 个触发点逐个「已接线调用点 / 未接线原因 / 接的时候要动哪里 / 业务价值排序」；四批推进计划与准入门槛（DoR/DoD）；§10 验收记录 |
| 前端：字典类型 | `frontend/src/api/business/flow/linkage.ts` | `LinkageTrigger.wired / wiring`、`LinkagePreviewRes.trigger_wired`、`RuleUpsertReq/RuleToggleReq.allow_unwired` |
| 前端：纯逻辑 | `frontend/src/views/business/flow/linkage/linkage-logic.ts` | `isWiredEntity`（fail-closed）/ `unwiredNote`（文案来自后端）/ `ruleWiringProblems` 与 `ruleOtherProblems`（两类问题分开）/ `previewAlertTheme`（未接线优先 warning）/ 下拉文案加「· 未接线」 |
| 前端：页面 | `frontend/src/views/business/flow/linkage/index.vue` | 顶部计数「已接线 3 / 未接线 12」；未接线触发点表单 warning + **第二道勾选**（不勾 ⇒ 保存 disabled）；列表「未接线」标签；影响预览 warning 条 |
| 独立验收脚本 | `scripts/probe_config_guard.py` | 13 条断言：字典 wired 3/12、写入口 400 硬门、`allow_unwired` 才放行、启用同样拦、预览 `trigger_wired` |
| 截图 | `shots/01..05*.png` | 配置页实测 5 张（见下） |

## 2. 三处一致性（这是「不漂移」的全部依据）

```
spec::TRIGGER_SPECS (Rust 常量)
   ├── spec::wired_triggers() ──► 单测 trigger_specs_cover_whitelist_in_order（3 个 / 12 个）
   ├── admin::catalog_triggers() ──► GET /business/flow/linkage 下发 wired + wiring（探针 A2/A4）
   ├── model::upsert_rule 硬门 ──► 未接线 + 启用 + 未 allow_unwired ⇒ 400（探针 B1/B3）
   └── 前端 triggerOptions / isWiredEntity ──► 下拉「· 未接线」+ 第二道勾选 + 保存 disabled（截图 01–04）
```

`tests_wiring::wired_triggers_match_call_sites` 反向扫描 `src/business` 全生产代码：**「调了引擎的文件集合」
必须恰好等于 `wired = true` 触发点的 `proofs` 文件集合** —— 删调用会红，偷偷多接一个不登记也会红。

## 3. 实测验收（本目录日志原文）

| 项 | 命令 | 结果 | 日志 |
|----|------|------|------|
| 后端单测（真库 `cloudpivot_lnk5c_arch_ut`，含 5 条接线守卫） | `ERP_DB_URL=… cargo test --lib business::flow::linkage -- --test-threads=1` | **59 passed / 0 failed，EXIT=0** | `logs/linkage_unit.txt` |
| 配置面探针（13 断言） | `LNK_PORT=18132 python3 scripts/probe_config_guard.py` | **PASS=13 FAIL=0** | `logs/probe_config_guard.txt` |
| 前端类型检查 | `pnpm typecheck` | **EXIT=0**（修掉上一版 4 个 `TS6133`） | `logs/frontend_typecheck.txt` |
| 前端逻辑单测 | `npx tsx --test tests/unit/flow-linkage.test.ts` | **9/9 pass，EXIT=0** | `logs/frontend_unit_linkage.txt` |
| 实例启动 / 迁移 | `scripts/p1fix5c_instance.sh start`（`cloudpivot_lnk5b` @18132） | `/health 200` · 迁移 0 个 · 联动授权 3 行 | `logs/server_startup.log` |
| 前端 dev server | `BACKEND_ORIGIN=http://127.0.0.1:18132 pnpm dev` | Vite 6.3.7 ready（5173） | `logs/vite_dev.log` |

截图（`shots/`，浏览器直连 5173 实测）：

1. `01-trigger-dropdown-unwired-labels.png` —— 下拉 15 项里 12 项带「· 未接线」；顶部「触发点已接线 3 个 / 未接线 12 个」
2. `02-form-unwired-warning.png` —— 选中 `STOCK_TRANSFER · 未接线` ⇒ 橙色 warning（正文即后端下发的 `wiring`）
3. `03-unwired-second-ack-blocks-save.png` —— 只勾第一道确认 ⇒「创建规则」**disabled**（不可静默保存）
4. `04-second-ack-enables-save.png` —— 勾上第二道确认才解禁（请求带 `allow_unwired: true`，后端仍会校验）
5. `05-draft-preview-rejects-unwired.png` —— 草稿预览红条「保存会被拒绝：触发点 STOCK_TRANSFER/DRAFT 尚未接线」

## 4. 复现

```bash
# 1) 起隔离实例（库 cloudpivot_lnk5b，端口 18132；二进制需含 wired 机制）
BIN=/root/p1fix5c-target/debug/cloudpivot REPO=/opt/cloudpivot \
  bash docs/evidence/20261105-p1fix5c-linkage-wiring/scripts/p1fix5c_instance.sh start

# 2) 配置面探针（13 断言）
LNK_PORT=18132 python3 docs/evidence/20261105-p1fix5c-architect-config-guard/scripts/probe_config_guard.py

# 3) 后端单测（需要真库；未设 ERP_DB_URL 会 SKIP = 假绿）
cd backend && ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_lnk5c_arch_ut' \
  cargo test --lib business::flow::linkage -- --test-threads=1

# 4) 前端
cd frontend && pnpm typecheck && npx tsx --test tests/unit/flow-linkage.test.ts
BACKEND_ORIGIN=http://127.0.0.1:18132 pnpm dev   # 浏览器开 /flow/linkage 看截图里的三处提示
```

## 5. 诚实边界

1. **只接 3 个，不是 15 个**：本卡不补接线（那是实现卡与后续分批卡的事），只保证「未接线的不会骗人」。
   12 个未接线触发点的接线动刀点逐条写在清单文档 §3.3。
2. **`wired` 只证明「有人调引擎」**：排除清单 / 上下文 / 幂等键是否正确，只能由端到端证据保证；
   本卡复用的是实现卡的 19 条 e2e 断言（`docs/evidence/20261105-p1fix5c-linkage-wiring/`），未重复贴。
3. **未跑 clippy**：本卡未改 Rust 源码；`cargo test` 编译通过间接证明可编译，clippy 由实现卡负责（其 README 记 EXIT 0）。
4. **生产 144 未部署**：本卡无迁移，发布只换二进制 + 前端。
