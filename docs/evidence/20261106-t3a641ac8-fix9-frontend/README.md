# P1-FIX-9 衍生 · P3：盘点详情凭证号 + 期初页默认期间（t_3a641ac8）

**执行**：frontend-developer · **时间**：2026-11-06 01:30–02:00（CST）
**被测树**：`/opt/cloudpivot` HEAD = `2cc6ba1` + 本卡未提交改动（清单见 §六）
**实例**：后端 `127.0.0.1:18191`（`/root/fix9b-bin/cloudpivot`，构建于 2026-11-06 01:35）· 前端 vite dev `127.0.0.1:15191` · 库 `cloudpivot_fix9b`（`cloudpivot_st9` 的克隆，**隔离实例，未碰生产**）

---

## 一、结论

| 项 | 结论 |
|---|---|
| F2 盘点详情「差异凭证 / 红冲凭证」 | ✅ 显示 `PZ-20261106-000007` / `PZR-20261106-000006`，可点开跳凭证列表按号核账；不再出现 `9256f0e…` |
| F3 期初页默认期间 | ✅ 首次进入落 **2026-11（当前月，已录 5,000.00）**，不再是空表 2026-12；2026-12 仍 OPEN 但「期初未录、合计 0.00」的对照截图一并留证 |
| `pnpm typecheck` | ✅ EXIT 0（`typecheck.txt`） |
| 前端单测（本卡两个套件） | ✅ 48/48 通过（`unit-tests.txt`） |
| 后端 `cargo test --lib stocktake` | ✅ 19/19 通过（`backend-tests.txt`） |
| 真机两轮 | ✅ 每轮 10/10 断言通过（`round1.txt` / `round2.txt`），截图各 6 张 |
| 写路径 / 状态机 | 未触碰（本卡只读 + 展示层） |

---

## 二、修法（比卡面「二选一」多走半步：后端回填 + 前端可点）

### F2 — 后端回填凭证号，前端显示并支持跳转

| 层 | 文件 | 改动 |
|---|---|---|
| 后端 DTO | `backend/src/business/erp/stocktake_dto.rs` | `StockTakeRes` 增 `voucher_no` / `reverse_voucher_no`（`Option<String>`）；`of()` 是**纯函数**（单测用假行直接调），拿不到库 ⇒ 默认 `None` |
| 后端 model | `backend/src/business/erp/stocktake_model.rs` | 新增 `voucher_no_map(ex, ids)`：`SELECT id, voucher_no FROM biz_voucher WHERE id = ANY(...) AND deleted_at IS NULL`，**一条 SQL 批量查**（一页 20 张单最多 40 个 id，无 N+1）；软删凭证不返回 |
| 后端 handler | `backend/src/business/erp/stocktake_handler.rs` | 新增 `fill_voucher_nos(&mut [StockTakeRes])`，`detail_of` 与 `page_of` **都**调用（列表 / 详情同口径，不会「详情有、列表没有」） |
| 前端契约 | `frontend/src/api/business/erp/stock-take.ts` | `StockTakeRow` 增 `voucher_no` / `reverse_voucher_no`（注释写清「查不到时为 null，不许用半截 UUID 冒充」） |
| 前端视图 | `frontend/src/views/business/erp/stock-take/index.vue` | 差异凭证渲染 `voucher_no`（`t-link`，点开 `router.push('/business/erp/voucher', {query:{voucher_no}})`）；**新增「红冲凭证」行**渲染 `reverse_voucher_no`，同样可点；两者都保留「查不到（已删除）」的兜底文案，不再把 `shortId(voucher_id)` 当凭证号 |

> 为什么两条都做：只用后端回填，财务拿到号还得自己去凭证页粘贴搜索；只用前端跳转，详情里永远看不到号（截图对比无从谈起）。号可读 + 一键核账才是这条 P3 的真实诉求。
>
> 凭证页深链的既有设计是「预填筛选 + 立刻查一次，然后把 `voucher_no` 从 URL 清掉」（`voucher/index.vue:openByQuery` 注释，避免刷新把筛选项改回去）——所以真机断言看的是**筛选框被预填 + 列表只命中这一张**，不是 URL 上还挂着参数。

### F3 — 默认期间：当前月优先，其次最早 OPEN，最后兜底

`frontend/src/views/business/erp/opening/opening-logic.ts` 新增纯函数 `defaultOpeningPeriod(rows, now)`（可单测），`opening/index.vue:loadPeriods` 改调它：

1. 当前月（北京时间 `todayPeriod()`）**且** `OPEN` → 用它（当期做账的绝大多数场景）；
2. 否则取**最早**的 `OPEN` 期间（`fetchPeriodPage` 是 `period_code DESC`，取排序后第一个 = 最早，不是「最新已开」）；
3. 一个 `OPEN` 都没有 → 兜底最新一条（只为展示状态与只读提示）。

**与卡面建议的一处口径偏差（诚实登记）**：卡面第二档写「最早一个**未录（NONE）**的 OPEN 期间」；本实现退一档为「最早 OPEN」。原因：期间清单接口不带「是否已录」字段，要判 NONE 得逐期间调 `GET /opening-balance`（首屏 N 个请求）。而「最早 OPEN」在业务上就是**正在被填的那一期**（期初按时间正序录），落点与「最早未录」在实际数据上一致；当前月存在时第一档直接命中，根本不走第二档。

---

## 三、真机验收（两轮，每轮 10/10）

脚本：`scripts/fix9b_e2e.cjs`（Playwright，真实浏览器 → 15191 → 18191 → `cloudpivot_fix9b`）
运行：`FIX9B_PASSWORD=<隔离库 admin 口令> node scripts/fix9b_e2e.cjs round1`（`round2` 同）——口令走环境变量，**仓库里不存明文**；完整起实例步骤见 `scripts/repro.sh`

| # | 断言 | round1 | round2 | 证据 |
|---|---|---|---|---|
| 1 | 期初页首次进入默认期间 = 当前月 2026-11 | PASS | PASS | `shots/roundN/01-opening-default-period.png` |
| 2 | 默认期间有数据（合计借方 5,000.00，非空表） | PASS | PASS | 同上 + `roundN.txt` |
| 3 | 对照：2026-12 也是 OPEN 但 0.00 / 期初未录（老逻辑的落点） | PASS | PASS | `shots/roundN/02-opening-period-2026-12-old-default.png` |
| 4 | 重新进入期初页仍默认当前月（不残留上一期） | PASS | PASS | `roundN.txt` |
| 5 | 盘点列表有已过账/已作废的单（`ST-20261106-000008`） | PASS | PASS | `shots/roundN/03-stocktake-list.png` |
| 6 | 差异凭证显示 `PZ-yyyyMMdd-nnnnnn` | PASS | PASS | `shots/roundN/04-stocktake-detail-vouchers.png` |
| 7 | 红冲凭证显示 `PZR-yyyyMMdd-nnnnnn`（不是只修一半） | PASS | PASS | 同上 |
| 8 | 不再出现 UUID 截断 `[0-9a-f]{8}…` | PASS | PASS | `roundN.txt` |
| 9 | 点差异凭证号 → 凭证列表预填该号、只命中 1 条 | PASS | PASS | `shots/roundN/05-voucher-list-from-stocktake.png` |
| 10 | 点红冲凭证号 → 同上，命中红字凭证 | PASS | PASS | `shots/roundN/06-voucher-list-from-stocktake-reverse.png` |

两轮界面输出逐字一致（除截图时间戳），round1 首轮曾因**首访引导气泡**（`.onboarding`）遮住「详情」链接点击失败——脚本已加 `hideOnboarding()` 隐藏该气泡（`round1` 目录里没有失败截图，失败轮次的临时截图未留），最终两轮全绿。

后端契约另有一份 HTTP 直读核对：`api-check.txt`（列表 + 详情 `voucher_no` / `reverse_voucher_no` 齐、6 个凭证号逐个在凭证列表按号检索 `total=1`、`FAILS: 无`）。

---

## 四、测试与静态检查（真实输出）

| 命令 | 结果 | 证据 |
|---|---|---|
| `pnpm typecheck` | EXIT 0 | `typecheck.txt` |
| `tsx --test tests/unit/erp-opening.test.ts tests/unit/erp-stock-take.test.ts` | 48/48 通过，EXIT 0 | `unit-tests.txt` |
| `cargo build --bin cloudpivot` | 成功（31.87s） | 见 §六 构建时间 |
| `cargo test --lib stocktake` | 19 passed / 0 failed，EXIT 0 | `backend-tests.txt` |

新增单测覆盖（不是「改完就算」）：

- `erp-opening.test.ts`：当前月 OPEN 优先（并**反向断言**老写法 `list.find(OPEN)` 在同一份数据上给出 2026-12）、当前月未开账时落最早 OPEN、全 CLOSED 兜底最新、空清单返回空串、输入顺序打乱仍同结果、**月边界按北京时间取月**（`2026-11-01 00:30 CST = 2026-10-31 16:30 UTC` 不能落 10 月）。
- `erp-stock-take.test.ts`：详情模板必须渲染 `voucher_no` / `reverse_voucher_no`、不得再出现 `shortId(voucher_id)` 当凭证号、必须带 `/business/erp/voucher` + `voucher_no:` 跳转。

### 既存红（与本卡无关，勿算在本卡头上）

`pnpm test:unit` **全量**当前是 835/837，2 个失败：

- `flow-biz-link.test.ts:173`「探测接口路径 = 后端 `#[nik::get(...)]` 注解」→ `src/business/task/handler.rs` 里已没有 `#[nik::get("/business/task/{id}")]`（任务模块 TD-2 拆分后该文件不再持注解）；
- `task-root-only.test.ts:89`「后端 dto / model / handler 三处与前端契约一致」→ 断言 `handler` 里有 `root_only: query.root_only.unwrap_or(false)`，同因拆分而找不到。

两个断言指向的 `backend/src/business/task/*` 与这两个测试文件在工作树里**零改动**（`git diff HEAD -- <paths>` 为空），即 HEAD 上就是红的，与本卡改动无交集。

---

## 五、环境指纹（可复现）

| 项 | 值 |
|---|---|
| 库 | `cloudpivot_fix9b` = `createdb -T cloudpivot_st9`（克隆，含 8 张盘点单 / 12 个 OPEN 期间 / 2026-11 期初 2 行 / 6 组盘点凭证） |
| 后端 | 18191，`NIK_PROFILE=test`，二进制 = `backend/target/debug/cloudpivot`（含本卡改动，01:35 构建） |
| 前端 | vite dev 15191，`BACKEND_ORIGIN=http://127.0.0.1:18191`，源码 = 当前工作树 |
| 账号 | `admin` / `st_wh`（克隆库内口令哈希被重置为本轮口令，仅隔离库；口令不入库文档） |
| 生产库 | **未触碰**（`cloudpivot` 生产库与 `cloudpivot_st9` 均无写入） |

## 六、改动清单（本卡 commit 只包含这些）

```
backend/src/business/erp/stocktake_dto.rs          (+11)
backend/src/business/erp/stocktake_model.rs        (+37)
backend/src/business/erp/stocktake_handler.rs      (+43/-1)
frontend/src/api/business/erp/stock-take.ts        (+10)
frontend/src/views/business/erp/stock-take/index.vue  (+46/-1)
frontend/src/views/business/erp/opening/index.vue     (+7/-3)
frontend/src/views/business/erp/opening/opening-logic.ts (+49)
frontend/tests/unit/erp-opening.test.ts            (+63)
frontend/tests/unit/erp-stock-take.test.ts         (+24)
docs/evidence/20261106-t3a641ac8-fix9-frontend/    （本目录）
```

工作树里另有 `doc_kind.rs` / `doc-kinds.ts` / `check.sh` / `payable_payment.rs` / `serial_handler.rs` 等**其他卡**的未提交改动，本卡**没有**纳入提交（`git commit -- <显式路径>`）。

## 七、遗留 / 未做

- 未跑盘点 **写路径**（建单/过账/作废）的端到端回归——按卡面「不碰写路径与状态机」，本轮只做只读展示验证；写路径两轮实测见父卡 `t_446586b6` 的 `docs/evidence/20261106-p1fix9-stocktake-opening/`。
- 凭证被软删时详情退化为「凭证 id xxxx（凭证号查不到，可能已删除）」——这是有意兜底（不拿半截 UUID 冒充凭证号），未做「点开正在删除的凭证」的跳转。
- F3 第二档口径见 §二（最早 OPEN 而非最早未录），如需严格「最早未录」，得让期间接口带上「是否已录」字段——那是后端改动，建议另行开卡。
