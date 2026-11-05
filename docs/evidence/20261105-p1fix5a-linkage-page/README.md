# P1-FIX-5A · 跨单据联动规则配置页（卡 t_d0daa4f0）

## 一句话

跨单据联动原来**只有内核、没有接口、没有页面**（`biz_flow_linkage_rule` 0 行是建表时的刻意设计，
`linkage::model` 的写入口也没有 HTTP 暴露）。本卡补齐了**配置管理面 HTTP + 配置页 + 迁移（菜单/权限）**，
真机跑通「配一条规则 → 列表可见 → 启停 → 预览影响 → 删除」，动作只能从**白名单下拉**里选，
参数表单由后端下发的声明驱动，页面上**没有任何可以写 SQL 的入口**。

## 交付物

| # | 文件 | 内容 |
|---|------|------|
| 1 | `backend/src/business/flow/linkage/admin.rs`（新增，约 900 行） | 配置管理面：`list_rules`（GET）/ `upsert_rule`（PUT）/ `toggle_rule`（PUT /toggle）/ `delete_rule`（DELETE）/ `preview_impact`（POST /preview）；`catalog_triggers`+`catalog_actions` 把 15 个触发点、11 个白名单动作及其参数声明下发给前端；4 条单测（字典覆盖 / implemented 标记 / 草稿校验 / 未知触发点状态） |
| 2 | `backend/src/business/flow/linkage.rs`（+2 行） | 声明 `pub mod admin;` + 模块表补一行 |
| 3 | `backend/migrations/business/V0253__flow_linkage_config_page.{up,down}.sql`（新增） | 页面节点 `/flow/linkage`（`business/flow/linkage/index`，挂在「审批流」目录 `…000860` 下，可见）+ 2 个权限码 `business:flow:linkage:{list,manage}`（只授 `SUPER_ADMIN`）+ 自检；down 全量回滚（不动规则表） |
| 4 | `frontend/src/api/business/flow/linkage.ts`（新增） | 5 个接口的类型与函数，契约逐字对齐 `admin.rs` |
| 5 | `frontend/src/views/business/flow/linkage/index.vue`（新增，约 1100 行） | 列表（触发点 / 动作 / 参数 / 顺序 / 启停 / 更新时间 / 操作）、筛选（关键词 + 源单据 + 只看启用）、新增 / 编辑弹窗（动态参数表单 + 白名单下拉 + 二次确认勾选）、启停与删除（DialogPlugin 二次确认）、影响预览弹窗（执行链 + 回滚风险 + 何时生效） |
| 6 | `frontend/src/views/business/flow/linkage/linkage-logic.ts`（新增） | 纯逻辑：动作下拉（未接管置灰）、参数表单 ↔ `action_params` 拼装（键集合 = 声明集合，结构上禁 SQL）、列表筛选、文案映射 |
| 7 | `frontend/tests/unit/flow-linkage.test.ts`（新增，9 条） | 单测纯逻辑（见下） |
| 8 | `frontend/src/constants/permissions.ts`、`frontend/src/router/index.ts` | 两个权限码常量 + `/flow/linkage` 路由（`meta.permission = business:flow:linkage:list`） |

### 关键函数

- `admin::list_rules`：含禁用行的规则列表 + 字典；未知 `trigger_entity` 直接 400（不静默返回空）。
- `admin::upsert_rule`：`begin()` → 内核 `model::upsert_rule`（白名单 / 状态 / 参数键与类型 / 二次确认 / 超管）→ commit / rollback，只做解析与错误透传。
- `admin::toggle_rule`：读原行 → 原样 upsert 只改 `enabled`（停用不是绕过校验的后门，留痕照走）。
- `admin::preview_impact`：按 `seq` 排执行链、判「同事务 + 关键动作 → 回滚风险」、跑草稿校验（与写入口同一份 `spec::validate_params`）。
- `index.vue::handleSave / handleToggleChange / handleDelete / openPreview`：写操作一律 `confirm: true`，弹窗里必须勾选确认框才能提交。
- `linkage-logic::buildParamPayload`：键集合只能等于 `action.params` 的键集合 —— 前端表单里没有「加一个键」的入口。

## 验收逐条

| 验收项 | 结果 | 证据 |
|--------|------|------|
| 配置页完成（列表 / 新增 / 编辑 / 启停） | ✅ | `shots/01-list.png`、`02-create-dialog.png`、`04-edit-dialog.png`；API 两条连跑：`logs/api_run1.txt`、`logs/api_run2.txt`（22/22 × 2） |
| 动作是白名单选择（不是自由输入） | ✅ | 下拉由 `GET` 下发 11 个动作，`implemented=false` 置灰（`linkage-logic::actionOptions`，单测 1）；后端对非白名单动作 400：「动作码「DROP TABLE」不在白名单里……不接受任何自定义语句（禁配 SQL）」；参数带 `sql` 键 400 |
| `pnpm typecheck && pnpm build` 通过 | ✅ | `logs/typecheck.txt`（EXIT=0）、`logs/frontend_build.txt`（EXIT=0） |
| 真机截图 4 张 | ✅ 5 张 + 1 张补充 | `shots/01-list.png`（列表）、`02-create-dialog.png`（新增 + 白名单下拉 + 动态参数）、`03-preview.png`（保存前影响预览）、`04-edit-dialog.png`（编辑，三项唯一键只读）、`05-preview-dialog.png`（执行链预览）、`01b-list-after-create.png`（创建后） |
| git commit | ✅ | 见卡内汇报的 commit |

### 额外自检（不在卡面验收里，但改了后端就应该有的）

- `cargo clippy --lib -- -D warnings` → EXIT 0（`logs/clippy.txt`）
- `rustfmt --edition 2024 --check` 两个改动文件无 diff
- 新增 4 条 Rust 单测全绿（`logs/rust_unit_tests.txt`：4 passed / 0 failed）
- 前端单测 9 条全绿（`logs/frontend_unit_tests.txt`：pass 9 / fail 0）
- 迁移 down → up 往返 + 幂等（`logs/migration_updown.txt`：DELETE 3 → INSERT 3，grants 3 行）
- 写配置留痕核对：`biz_status_log(entity_type=FLOW_LINKAGE_CFG)` 一行一条，`reason` 里是改前/改后快照

## 真机环境

- 库：`cloudpivot_lnkfe`（从零跑完整迁移链，V0253 由启动时迁移器实际执行）
- 后端：隔离实例 `127.0.0.1:18121`（`/root/lnkfe-instance.sh`，profile=test）
- 前端：vite dev `127.0.0.1:5197`，`BACKEND_ORIGIN=http://127.0.0.1:18121`
- 账号：`admin`（bootstrap 初始口令在隔离环境里改成验收口令，脚本可重入）

## 复现步骤

```bash
# 1) 后端（从零建库后启动即自动迁移，含 V0253）
bash /root/lnkfe-instance.sh start

# 2) 接口验收（两轮，脚本自带复位，起点一致）
python3 /root/lnkfe-api-test.py     # 22/22 PASS
python3 /root/lnkfe-api-test.py     # 22/22 PASS

# 3) 前端
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18121 pnpm dev --port 5197 --strictPort
# 4) 截图（需把 lnkfe-shots.mjs 放在 frontend/ 下，才能解析 @playwright/test）
```

## 偏差 / 需要知道的事

1. **本卡不得不补后端接口**：`linkage::model` 只有函数、没有 handler / 路由（`grep -rn linkage backend/src --include=*.rs -l` 里没有任何 handler），
   前端页面没有 API 就只能是空壳。补的是**薄层**（约 900 行，其中一半是 DTO 与文档），校验 / 写库 / 审计全部复用内核
   `model::upsert_rule` / `delete_rule`，没有第二套业务逻辑。同时新增 V0253 种菜单与权限码，否则页面没有入口。
2. **只授 `SUPER_ADMIN`**：与内核 `ensure_config_admin` 同口径（联动规则决定「单据状态变了以后还会动哪些账」）。
   非超管角色要在「角色管理」里勾选 `business:flow:linkage:list` 才看得到页面。
3. **空库首启的授权窗口**：`sys_role` 由 bootstrap 在**迁移之后**创建，所以从零建库时 V0253 的 `sys_role_menu` 是空操作
   （与 V0192/V0197/V0202/V0206 同一套已知口径）。本卡在隔离库里 bootstrap 之后重放了 V0253（幂等，`INSERT 0 3` 只补授权），
   生产库（角色已存在）不受影响。
4. **动作白名单里只有 3 个已接管**：`SEND_NOTICE` / `UPDATE_RECEIVED_QTY` / `GENERATE_VOUCHER`；其余 8 个在页面里置灰
   （后端也会 400）。这是内核 `spec.implemented` 的现状，不是本卡的裁剪。
5. **规则不回放历史单据**：页面与预览里都明说了——只有之后发生的状态变更才会执行联动。
6. **浏览器控制台有一条 401**：`GET /api/business/notice/stream`（SSE 长连接，EventSource 不带 Authorization）。
   与本页无关，属全站既有行为，未在本卡范围内改动。
7. **未部署生产 144**：卡面只要求配置页 + typecheck/build + 截图 + commit；上线走 `scripts/deploy/deploy.sh`
   （V0253 会随迁移链一起执行）。
