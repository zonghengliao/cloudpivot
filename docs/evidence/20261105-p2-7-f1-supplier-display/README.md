# 证据 · P2-7-F1 采购单供应商名口径对齐（现值 → 快照回落，禁止落 uuid）

> 卡：`t_f5fbd004`（父卡 `t_54ad4c5f`【P2-7】供应商三名不一致）
> 日期：2026-11-05 · 主机 99 · 隔离实例 `127.0.0.1:18171` + 隔离库 `cloudpivot_p27f1` + 前端 dev `127.0.0.1:5181`
> 口径真源：`docs/编码规范.md` §1.10（S2/S3）+ `docs/architecture/P2-7-供应商名口径盘点.md` §3

## 0. 一页结论

| 验收项 | 结果 | 证据文件 |
|---|---|---|
| `pnpm typecheck` 0 error | 退出码 0，无任何诊断 | `typecheck.txt` |
| 新增纯逻辑单测全绿 | **12/12 pass**（含「断言输出不含 uuid 短号」） | `unit-p2-7.txt` |
| 全量单测（740 条） | 738 pass / **2 fail，且这 2 条在干净 HEAD 上同样红**（凭证审批进度命名漂移，与本卡无关） | `unit-full.txt`、`pre-existing-c2-red-at-HEAD.txt` |
| 改名场景：列表显示**新名** | 供应商改名 `示例供应商（种子）` → `华信智造`，列表两行都变成 `华信智造` | `step-2-after-rename.json`、`ui-1-rename-list.png` |
| 改名场景：详情并列「现名 / 快照」 | 现名 = `华信智造`，下单时快照 = `示例供应商（种子）`（灰字） | `ui-2-rename-detail.png` |
| 主档软删场景：列表显示**快照名**（不是 uuid） | `DELETE /erp/supplier/{id}` 软删后主档清单 0 条，列表回落 `示例供应商（种子）`，DOM 里 uuid 形态命中数 0 | `step-3-after-soft-delete.json`、`ui-3-soft-deleted-list.png` |
| 主档软删场景：详情 | 现名回落快照 `示例供应商（种子）`、快照栏同值；**没有任何 uuid** | `ui-4-soft-deleted-detail.png` |
| 后端 DTO / 其它页面 / 历史快照数据 | **一行未改**（改动只有 2 个前端文件 + 1 个新单测 + 本证据目录） | 见 §3 改动清单 |

## 1. 改了什么（口径落点）

| 位置 | 改前 | 改后 |
|---|---|---|
| `frontend/src/views/business/erp/purchase/index.vue:118`（列表列） | `supplierNameOf(row.supplier_id)` | `supplierDisplayOf(row)` = **主档现值 → `row.supplier_name`（快照）→ `—`** |
| 同上 `supplierNameOf` 实现 | `suppliers.find(...) ?? shortId(supplierId)`（主档软删 → 显示 uuid 短号） | 改为 `supplierCurrentNameOf()`（查不到给 `—`），**`shortId` 兜底整条删除** |
| 同上 详情抽屉「供应商」栏 | 单行现值，无快照 | **并列两行**：`供应商（现名）` + `下单时供应商快照`（只读灰字、无则 `—`），照抄合同详情 `contract/index.vue:405-409` |
| `purchase-logic.ts` | 无 | 新增 `supplierDisplayName()` / `supplierCurrentNameOf()`（纯函数，单测直接覆盖） |

设计取舍：列表是「看现在」的地方 → **现值优先**（改名后必须跟着变，否则同一供应商在两张单上两个名字，对账会误判成两家）；
审计「当时叫什么」由详情页并列的快照栏承担（S3）。这与父卡 §3.1 的拍板一致。

## 2. 实测（隔离环境，真实数据）

环境：`cloudpivot_p27f1`（空库 → 后端启动自动迁移 + 种子）→ `backend/target/debug/cloudpivot` 起在 `18171` →
`npx vite --port 5181`（`BACKEND_ORIGIN=http://127.0.0.1:18171`）。数据全部是本机隔离库，**未碰生产、未碰别人的库**。

三步口径断言（`collect.py` 自己判，`checks.json` 为真机输出，**全 true**）：

```json
{
  "step1_快照=下单时现值": true,
  "step2_主档现值=新名": true,
  "step2_单据仍下发旧名快照": true,
  "step3_主档清单已不含该供应商": true,
  "step3_列表/详情仍下发旧名快照（非 uuid）": true,
  "step3_后端确实软删": true
}
```

逐步：

1. **建单**（`POST /erp/purchase-order`，现货采购，供应商 = 种子供应商）→ `supplier_name_snapshot = 示例供应商（种子）`
   （`step-1-after-create.json`：DB 行、列表行、详情行三者一致）。
2. **改名**（`PUT /erp/supplier/{id}`，`supplier_name = 华信智造`）→ 主档现值变新名，**采购单下发的 `supplier_name` 仍是旧名**
   （`step-2-after-rename.json`：`master_current_name = 华信智造`，`list_row/detail_row.supplier_name = 示例供应商（种子）`）。
   前端此时显示：列表 = `华信智造`（现值命中），详情 = 「现名 华信智造」+「快照 示例供应商（种子）」。
3. **主档软删**（`DELETE /erp/supplier/{id}`，真实接口，本版后端未拦被草稿单引用）→ `supplier/page` 返回 0 条，
   前端 Map 未命中 → 列表回落快照 `示例供应商（种子）`，详情两栏都是 `示例供应商（种子）`。

浏览器里取到的 DOM 文本（`browser_exec` 实测，不是手写）：

```text
# 改名态（主档现值 = 华信智造）
LIST: [{"row":"PO-20261105-000004","supplier":"华信智造"}]
DETAIL: {"cur":"华信智造","snap":"示例供应商（种子）"}

# 主档软删态（supplier/page → n=0）
LIST: {"rows":[{"row":"PO-20261105-000004","supplier":"示例供应商（种子）"}],"uuidLike":[]}
DETAIL: {"cur":"示例供应商（种子）","snap":"示例供应商（种子）"}
```

截图（1280→1500 宽视口，表格供应商列完整可见）：

| 文件 | 现场 |
|---|---|
| `ui-1-rename-list.png` | 改名后列表：供应商列 = `华信智造`（现值） |
| `ui-2-rename-detail.png` | 改名后详情：`供应商（现名）华信智造` + `下单时供应商快照 示例供应商（种子）`（灰字） |
| `ui-3-soft-deleted-list.png` | 主档软删后列表：供应商列 = `示例供应商（种子）`（快照，无 uuid） |
| `ui-4-soft-deleted-detail.png` | 主档软删后详情：现名回落快照 + 快照栏同值 |

## 3. 改动清单（提交只含这些路径）

```text
frontend/src/views/business/erp/purchase/index.vue          # 列表列 + 详情并列两行 + supplierNameMap
frontend/src/views/business/erp/purchase/purchase-logic.ts  # supplierDisplayName / supplierCurrentNameOf
frontend/tests/unit/p2-7-supplier-display-name.test.ts      # 12 条（含 uuid 反向断言 + 源码契约）
docs/evidence/20261105-p2-7-f1-supplier-display/            # 本目录（含 collect.py 可复跑）
```

未改：后端任何 DTO/处理器、`stockin` / `payable` / `contract` 页面、`biz_purchase_order.supplier_name_snapshot` 历史数据。

## 4. 复跑

```bash
# ① 空库 + 起后端（自动迁移 + 种子）
su - postgres -c "createdb -O alun cloudpivot_p27f1"
cd /opt/cloudpivot/backend
env NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_p27f1 NIK_DATABASE_HOST=127.0.0.1 \
    NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_SERVER_LISTEN=127.0.0.1:18171 \
    NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
    CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
    ./target/debug/cloudpivot

# ② 口径三步（自带断言，失败会非 0 退出；口令走环境变量，不落仓）
export P27_INITIAL_PASSWORD='<空库迁移时 NIK_ADMIN_PASSWORD 的值>'
export P27_APP_PASSWORD='<本脚本要改成的口令>'
python3 docs/evidence/20261105-p2-7-f1-supplier-display/collect.py

# ③ 门禁
cd frontend && pnpm typecheck && npx tsx --test tests/unit/p2-7-supplier-display-name.test.ts

# ④ 真机 UI（可选）
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18171 npx vite --port 5181
```

> `collect.py` **不含任何口令字面量**：它对着本机一次性隔离库跑，管理员口令从 `P27_INITIAL_PASSWORD` /
> `P27_APP_PASSWORD` 环境变量读；首次登录会被后端拦（403「首次登录必须先修改初始密码」），脚本自己完成改密。
> 生产不跑这个脚本。

## 5. 已知与本次无关的红（诚实记录）

`pnpm test:unit` 全量 740 条里 **2 条常红**，都在 `tests/unit/c2-approval-unified.test.ts`：

```text
not ok 21 - ③ 凭证详情：底部新增「审批进度」区块（节点 + 审批人明细），位置在分录 / 合计之后
not ok 22 - ③-补 凭证：进度数据源是 `pending_reverse_instance_id`（P-FLOW-3B 下发的字段），不是自造 id
```

取证方式：`git archive HEAD frontend` 导出**干净提交树**到临时目录，同一份 node_modules 下跑这条用例 →
**同样红、同样的 2 条**（`pre-existing-c2-red-at-HEAD.txt`）。即：该红来自 `voucher/index.vue` 已统一到
`approvalProgress`、而测试仍断言历史名 `reverseProgress`，与本卡改动无交集（本卡只碰 purchase 目录）。
按「不越界改别人页面」的边界，这里只记录不复修。
