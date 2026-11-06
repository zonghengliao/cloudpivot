# SO-03 客户档案前端页面 · 验收证据（2026-11-06）

卡：`t_3f4c05ae`（【SO-03】客户档案前端页面）· 上游：`t_1cc778e9`（SO-02 后端 API + V0261 权限码，提交 `7b05716`）

## 一、交付物

| 类型 | 文件 | 说明 |
|------|------|------|
| 页面 | `frontend/src/views/business/erp/customer/index.vue` | 列表（编码/名称/联系人·电话/信用额度/状态/操作）+ 分页 + 服务端搜索 + 新建/编辑弹窗（全字段 + 校验）+ 状态切换 + 软删确认 |
| 纯逻辑 | `frontend/src/views/business/erp/customer/customer-query.ts` | 列定义、筛选入参、表单回填、校验、新建/编辑/状态切换请求体（可单测） |
| API | `frontend/src/api/business/erp/index.ts`（追加 `// 销售链：客户档案（SO-03）` 段） | `Customer` / `CreateCustomerReq` / `UpdateCustomerReq` + 5 个函数（page/detail/create/update/delete） |
| 路由 | `frontend/src/router/index.ts` | `path: 'business/erp/customer'`、`name: 'BusinessErpCustomer'`、`meta.permission = business:erp:customer:list` |
| 单测 | `frontend/tests/unit/erp-customer.test.ts` | 19 条：列名逐字对齐后端 / 字符串状态域 / 部分更新只发改动字段 / 清空发空串 / 额度裁尾零 |
| 迁移 | `backend/migrations/business/V0262__sales_customer_menu_page.{up,down}.sql` | 把 V0261 故意留空的 `path`/`component`/`is_visible` 补成终态（`/business/erp/customer` / `business/erp/customer/index` / TRUE） |
| 迁移 | `backend/migrations/business/V0263__sales_customer_menu_label.{up,down}.sql` | 真机截图发现侧边栏被长 title 截断（`客户档案（销售链交易对手…`）→ 显示名收敛成 `客户档案`（与 供应商 / 仓库管理 同口径） |
| 验证脚本 | `scripts/verify/so03_menu_rollback.sql` | 事务内跑两个 down → 断言页面回到 NULL/NULL/FALSE → 再跑两个 up → ROLLBACK（演练不留痕） |

## 二、门禁与实测（全部真跑，日志见同目录）

| 项目 | 命令 | 结果 | 证据 |
|------|------|------|------|
| 类型检查 | `pnpm typecheck` | rc=0（无输出即无错） | `typecheck.txt` |
| 生产构建 | `pnpm build` | `vue-tsc --noEmit` + `vite build` → **✓ built in 46.39s** | `build.txt` |
| 前端单测（全量） | `pnpm test:unit` ×2 | **856 passed / 0 failed**（连跑两轮一致） | `unit-tests.txt` |
| 前端单测（本卡） | `npx tsx --test tests/unit/erp-customer.test.ts` ×2 | **19 passed / 0 failed** | `unit-tests.txt` |
| 菜单路由门禁 | `SYS_DB_URL=…cloudpivot_v0260 cargo test --lib system::menu` ×2 | **20 passed / 0 failed**（含 n08） | 见 §四 |
| n08 明细 | 同上 `--nocapture` | `[N08] 97 条菜单路径全部能在前端路由里解析 ✓` | 见 §四 |
| 死权限码闸 | `python3 scripts/verify/perm_menu_gate.py` | PASS | 见 §五 |
| 孤儿按钮闸 | `python3 scripts/verify/orphan_button_gate.py` | 孤儿 (角色,码) = 0 | 见 §五 |
| 回滚演练 | `psql -f scripts/verify/so03_menu_rollback.sql` | down → 页面关闭 → up → 终态 → ROLLBACK 无痕 | 见 §六 |
| 真机 UI | `node verify.mjs 1` / `verify.mjs 2`（Playwright + vite dev + 隔离后端 18097） | **两轮各 31 PASS / 0 FAIL** | `round-1.txt` / `round-2.txt` + 10 张截图 |

## 三、真机 UI 验收（两轮，逐项）

环境：`vite dev :5186`（`BACKEND_ORIGIN=http://127.0.0.1:18097`，源码即产物）+ 隔离后端 `127.0.0.1:18097`（库 `cloudpivot_v0260`，已应用 V0260/V0261/V0262/V0263）+ 真 token（`admin`，从 `/api/system/auth/login` 取，写文件不进日志）。

每轮用**唯一编码**（`SO03-E2E-<ts>`）造数据，跑完删除并复核残留：

1. **列表**：seed 3 条客户渲染；口径「共 3 条」；`CUS-0003`（额度 0）显示**「不控额度」**而不是 `0`；状态 tag「正常」；分页器（共 3 条数据 / 1 页）→ `round-N-1-list.png`
2. **搜索**：编码模糊 `CUS-0002` → 只剩 1 行；搜不存在编码 → 空态「没有匹配的客户」；「重置」回全量
3. **新建**：空表单点「创建」→ 字段红字「客户编码必填」且**未发 POST**（校验拦在请求前）；填全字段提交 → 列表出现新编码；回读接口：`name`/`contact_name` 落库、`credit_limit = "12345.6700"`（字符串定点）、`status = ACTIVE`（默认）→ `round-N-2-create-dialog.png`
4. **编辑**（部分更新）：改名 / 改联系人 / 改额度 → 列表与接口同步；**未改的税号保持不变**（证明不是整表单覆盖）→ `round-N-3-edited.png`
5. **状态切换**：点「冻结」→ 行内 tag「冻结」+ 库 `status=FROZEN`；点「解冻」→ tag「正常」+ 库 `status=ACTIVE`（请求体只有 `{status}`）→ `round-N-4-status.png`
6. **删除**：确认弹窗文案写明「同一编码可以再次使用」；删除后列表消失、分页 `total=0`、详情 404（软删）→ `round-N-5-deleted.png`
7. **噪声**：页面无未预期 console error（仅排除全站既有的 `notice/stream` 401）

两轮 API 流水（真接口，`round-N.txt` 尾部）：
`GET page×4 → POST → GET page → PUT(编辑) → GET → PUT(冻结) → GET → PUT(解冻) → GET → DELETE → GET`，全部 200。

## 四、菜单路由门禁（真库档）

```
SYS_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_v0260 cargo test --lib system::menu
  ROUND 1: test result: ok. 20 passed; 0 failed
  ROUND 2: test result: ok. 20 passed; 0 failed
cargo test --lib system::menu::tests::n08 -- --nocapture
  [N08] 97 条菜单路径全部能在前端路由里解析 ✓  → 1 passed
```

n08 读的是 `backend/src/system/menu/tests.rs` 里 `env!("CARGO_MANIFEST_DIR")/../frontend/src/router/index.ts` 的 `path: '...'`，
统一补 `/` 后与 `sys_menu.path` 整串比对 —— 所以本次迁移写的 `/business/erp/customer` 必须与路由项**逐字**相同。

## 五、权限闸

```
python3 scripts/verify/perm_menu_gate.py     → PASS（4 个 customer 码都有 sys_menu 承载行）
python3 scripts/verify/orphan_button_gate.py → ✓ 孤儿 (角色,码) = 0
```

本卡**未新增/未删除任何权限码**（4 个码由 V0261 种下），也未写 `sys_role_menu`（授哪些角色是管理决策，与 V0250/V0261 同口径）。

## 六、迁移与回滚

```
version | name                      | kind      | applied_via | checksum(=up.sql 的 sha256，逐字相等)
262     | sales_customer_menu_page  | versioned | executed    | 1ce2400b1115…
263     | sales_customer_menu_label | versioned | executed    | 2f3c117c2df7…
```

回滚演练（`scripts/verify/so03_menu_rollback.sql`，单事务）：

```
V0263 down → title 还原长文案（改 1 行）
V0262 down → path/component=NULL、is_visible=FALSE；3 个按钮与 4 个权限码保留（改 1 行）
down 断言通过
V0262 up → 重新开页（改 1 行）；V0263 up → 短标题（改 1 行）
3 个按钮仍挂在页面节点下（…2602/2603/2604）
ROLLBACK → 与演练前完全一致（无痕）
```

## 七、复现方法

```bash
# 1) 隔离后端（应用迁移 + bootstrap）
bash scripts/verify/so02_start.sh start            # 库 cloudpivot_v0260，端口 18097

# 2) 前端（源码即产物，别在 dist 上验）
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18097 ./node_modules/.bin/vite --port 5186 --strictPort

# 3) 取真 token（口令/令牌只落文件，不打印）
OUTDIR=~/.cache/so03 python3 scripts/verify/so03_login.py

# 4) 两轮真机验收（Playwright；ESM 按**脚本所在目录**解析包，所以要放到有 node_modules 的目录）
mkdir -p ~/.cache/so03 && cp scripts/verify/so03_customer_ui_e2e.mjs ~/.cache/so03/
ln -sfn "$PWD/frontend/node_modules" ~/.cache/so03/node_modules
SO03_SCRATCH=~/.cache/so03 node ~/.cache/so03/so03_customer_ui_e2e.mjs 1
SO03_SCRATCH=~/.cache/so03 node ~/.cache/so03/so03_customer_ui_e2e.mjs 2

# 5) 门禁
cd frontend && pnpm typecheck && NODE_OPTIONS=--max-old-space-size=8192 pnpm build && pnpm test:unit
cd backend  && SYS_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_v0260 cargo test --lib system::menu
```

> 提交进仓库的 `scripts/verify/so03_customer_ui_e2e.mjs` 是**同一份脚本**：本卡在 scratch 里以 `repro` 轮
> 单跑过一遍（**31 PASS / 0 FAIL**），证明这份拷贝可复跑，不是事后补的伪代码。

> `pnpm build` 在 16G 机器上默认堆（~4G）会 `FATAL ERROR: Reached heap limit`（同事同时跑 cargo 时更容易），
> 本卡实测加 `NODE_OPTIONS=--max-old-space-size=8192` 后 46s 通过；这是**环境**问题，不是本页代码问题
> （同一命令在加堆参数前后代码一字未改）。

## 八、已知边界（诚实声明）

- 仍**没有授权**任何角色看客户档案（V0261/V0262 都不写 `sys_role_menu`）⇒ 除超管外，菜单不可见、接口 403。
  授哪些角色（销售员可写 / 财务只读）是管理决策，留给后续发布/权限卡。
- `biz_customer_contact`（联系人子表）后端还没有接口，本页只读写客户主档上的 `contact_name` / `contact_phone`
  （与 SO-02 交接一致）。
- prod（144）/ `cloudpivot_test` 尚未应用 V0262/V0263，走发布卡。
- 隔离实例仍在 `127.0.0.1:18097`（`bash scripts/verify/so02_start.sh stop` 可停）；vite dev `:5186` 验完已关。
