# E2E-FIX-6 证据：批量生成落「默认库位桶」（一物一码）

卡片：`t_25496880` · E2E-FIX-6【P3】一物一码批量生成不落默认库位桶（与收货/期初不同桶）
提交：`7a3c4d21`（`origin/main`）

## 1. 缺陷与根因

```text
POST /business/erp/serial/batch-generate 不传 location_id
  → 成本账 biz_inventory.location_id = NULL、库位账 = NULL、序列号 location_id = NULL（「未分配」桶）
收货 / 期初
  → 落「默认库位桶」（biz_location.is_default = 1，ADR-20261031）
⇒ 同物料同仓必然两桶并存：库位库存页多一行「(未分配)」、扫码看不到库位、逐桶对账对不上。
```

根因有两段：

1. **前端** `serial-logic.ts::buildBatchPayload` 根本不发库位（表单也没有这个字段）；
2. **后端** 只认「库位账行 id」（`biz_inventory_location.id`）——那是过账后才存在的东西，
   前端在下拉里选的是「库位字典」（`biz_location.id`），两个 id 空间不同，前端无论如何都传不对。

## 2. 修法

| 层 | 改动 |
|---|---|
| DTO | `BatchGenerateReq` 新增 `location_dict_id`（`biz_location.id`）；与历史 `location_id`（账行 id）互斥，同时给 ⇒ 400 |
| handler | 校验库位字典：未软删 / 属于所选仓库 / 已启用（文案与收货同口径）；校验在**取号之前** |
| model | 新增 `serial_model::ensure_batch_location_row`：传字典 → 按 (物料, 仓库, 库位) find-or-create 账行；不传 → 该仓默认库位（`stockin_location::default_location_in`，与收货同一函数）；该仓无默认库位 → `None`（未分配桶，收货也允许） |
| 事务 | 建账行在**同一个事务**里做，失败与取号 / 记账 / 凭证一起回滚，不留空账行 |
| 前端 | 序列号管理页「批量生成」弹窗新增「入库库位」下拉：按仓库加载、只列启用、默认带出该仓默认库位；切仓时保留仍属于新仓的已选库位；提交带 `location_dict_id`；成功提示点明本次入库库位 |
| 存量 | `scripts/datafix/p15/`：只读报告 + 合并脚本（未分配桶并入默认库位桶，仓级合计守恒校验，无默认库位的仓跳过，不可自动回滚） |

## 3. 同一次提交里的口径更正（不涉及行为变更）

`t11_t12_t20_mixed_buckets_keep_b1_and_one_txn_per_movement` 这条混桶用例特意用「**不传库位**」造
NULL 汇总桶；本卡把「不传」重新定义成「按仓库默认库位兜底」之后，该用例的前提必须保持不变：

- 夹具**没有**给 `WH-W2B-1` 配默认库位（给 `WH-W2B-1` 配默认库位会改掉那条用例的语义）；
- 本卡新增独立仓库 `WH-W2DEF`（唯一有默认库位的仓）+ 两个库位（默认 / 非默认）承载新用例。

## 4. 测试与演练结果

### 4.1 后端真库单测（99，库 `cp_e2efix6`，`--test-threads=1`）

```
business::erp::serial  →  73 passed; 0 failed   （新增 5 条，见 4.3）
business::erp::stockin → 163 passed; 0 failed   （收货路径回归：默认库位解析复用的是它的函数）
```

### 4.2 前端

```
pnpm typecheck  → exit 0
pnpm build      → ✓ built in 51.59s
单测 erp-serial-list.test.ts → 11/11 passed（新增 2 条：请求体带 location_dict_id、默认库位带出/切仓保留）
```

### 4.3 新增的单测（`serial_handler.rs::tests`）

| 用例 | 钉住什么 |
|---|---|
| `batch_generate_without_location_lands_in_default_bucket` | 不传库位 → 默认库位账行 + 成本账 1 个桶（无 NULL 桶）+ 库位账一致 + **扫码带库位名** |
| `batch_generate_with_location_dict_uses_that_bucket` | 传库位字典 → find-or-create 该库位账行，默认桶不动 |
| `batch_generate_rejects_location_of_other_warehouse` | 别的仓的库位 → 400，且序列号一条不落（校验先于取号） |
| `batch_generate_without_default_location_falls_back_to_unassigned` | 仓库无默认库位 → 未分配桶（与收货同口径，不编造库位） |
| `batch_req_rejects_both_location_fields` | `location_id` + `location_dict_id` 同时给 → 400 |

### 4.4 真实 HTTP 演练

- `live-99.txt`：99 隔离实例 `127.0.0.1:18420` / 库 `cp_e2efix6`（空库首启，全量迁移 + 种子）→ **PASS=49 FAIL=0**
- `live-145.txt`：145 测试机隔离实例（私有 worktree `/root/e2efix6-wt` + 私有 target `/root/e2efix6-target`，
  库 `cp_e2efix6`，端口 18420）同脚本同断言 → **PASS=49 FAIL=0**
- `raw-99/`、`raw-145/`：两台机每一步的原始请求体 + 响应体
  （`batch_*.json` / `inventory_location_page_*.json` / `scan_*.json` / `inventory_audit.json` / `p15_*.json`）

> **演练抓出来的一个真 bug（已修）**：P15 合并脚本第一版只清了**成本账**的未分配桶，
> 没清**库位账**的未分配行 ⇒ 合并后成本账 0 / 库位账 2，B1b 逐桶对账错位。
> 修法是第 ⑥ 步把库位账的未分配行一并清零，并在演练里加了一条
> 「合并后 B1b 逐桶 0 差异」的断言（就是这条断言把它抓出来的）。

> 145 上踩到的一个坑（记下来省下一个人 20 分钟）：`config/config-test.toml` 是**本机未跟踪文件**，
> 干净的 worktree 里没有它 ⇒ `[middleware.auth] enabled = false` 生效、`jwt_auth=false`，
> 所有 `#[nik::permission]` 接口和 `PUT /system/auth/password` 都报
> `Missing request extension AuthClaims`（500）。把 99 的 `config-test.toml` 拷进 worktree 再起实例即可。

演练覆盖（对应卡面验收）：

| 验收 | 演练里怎么证明 |
|---|---|
| ① 表单有「入库库位」、默认带出仓库默认库位 | 前端单测（`defaultLocationIdOf` / `locationAfterWarehouseSwitch` / `buildBatchPayload`）；页面下拉按仓库加载、默认选中默认库位 |
| ② `/inventory-location/page` 落默认库位桶 | 生成后查该物料该仓：**1 行**，`location_id = 默认库位`，`qty = 3`（不是 null） |
| ③ 直接调 API 不传 `location_id` | 请求体只有 `material_id/warehouse_id/count/unit_cost`；成本账只有 1 个桶、未分配桶 qty=0 |
| ④ 扫码能看到库位 | `GET /serial/scan?serial_no=…` → `location_name = "E2F6 默认库位"`、`location_id` = 默认库位账行 |
| ⑤ 巡检不再报该物料的假差异 | `GET /inventory/audit?dry_run=true` 明细里没有该物料；逐桶 B1b（成本账 = 库位账）0 差异；不变式① 两仓都成立 |
| ③ 存量两桶 | 造旧代码形态（未分配桶 qty=2 + 2 件裸 NULL 序列号）→ P15 报告点名 → 合并 → 未分配桶清零、默认桶 3+2=5、仓级合计守恒 7=7、在库序列号 0 件无库位、报告复查不含该仓、巡检仍不报 |

## 5. 复现命令

```bash
# 99：起隔离实例（空库首启自动迁移 + 种子）
bash docs/evidence/20261109-e2efix6-serial-location/scripts/00_start_instance.sh start
# 演练（真实 token）
E2F6_BASE=http://127.0.0.1:18420 E2F6_DB=cp_e2efix6 E2F6_ADMIN_PW='Admin@123456' \
  python3 docs/evidence/20261109-e2efix6-serial-location/scripts/01_drill.py
```

## 6. 已知边界（没做 / 没验）

- **没跑全量 `cargo test --lib`**（3882 条）：只跑了受影响的 `business::erp::serial`（73）与
  `business::erp::stockin`（163）。全量并发在同库上本来就会跨模块串（见
  `docs/测试卫生-真库单测分库跑法.md`）。
- **没做浏览器点击级 E2E**：下拉的默认带出 / 切仓保留 / 请求体形态由 `erp-serial-list.test.ts` 覆盖；
  表单渲染本身没在 Playwright 里点过。
- **144 生产未碰**（卡面禁止）；145 用**隔离实例**（私有 worktree + 独立库），没有重启共享的
  `cloudpivot-test.service(:8033)`。
- P15 合并脚本对「没有默认库位的仓」有意跳过（收货也落未分配桶，属于正常形态），
  报告里会继续列出这些行 —— 这是判据而不是遗漏。
