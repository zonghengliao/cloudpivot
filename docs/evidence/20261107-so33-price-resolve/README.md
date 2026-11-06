# SO-33 取价解析接口（5 级确定性命中）—— 交付证据

卡：`t_9d503366` · 依据：`docs/architecture/ARCH-7-销售模块设计.md` §15.3.3 ·
库：`cloudpivot_so33`（`cloudpivot_so32` 克隆，V0306 → 启动时补 V0307）· 端口 18239

## 1. 接口

```
GET /api/business/erp/price/resolve?customer_id=&material_id=&qty=&date=
权限码 business:erp:price:resolve（V0307 已播种）

响应 {code, msg, data:{ price, source, source_id }}
  price     : Decimal(18,4)，JSON 里是**字符串**；未取到价 = null
  source    : AGREED | TIER_CUSTOMER | TIER_COMMON | STANDARD | NONE
  source_id : 命中行主键；STANDARD = 物料 id；NONE = null
```

| 参数 | 必填 | 口径 |
|------|------|------|
| `material_id` | ✅ | UUID，非法/缺省 → 400 + 10001 |
| `qty` | ✅ | 定点数 ≥ 0；**缺省不按 0 处理**（会静默取「无起订量」档的偏低价）→ 400 |
| `customer_id` | ❌ | 不传/空串 = 跳过 ①②，只走 ③④⑤ |
| `date` | ❌ | `YYYY-MM-DD`；缺省 = 今天（北京时间，`voucher::today_cst` 同一把尺） |

## 2. 优先级与**并列打破**（卡面要求「确定性」，这里补全了卡面没写的部分）

```
① biz_customer_price            min_qty <= qty + 有效期覆盖 → min_qty DESC → effective_from DESC → id ASC
② biz_price_tier customer!=null min_qty <= qty < max_qty + 有效期 → PROMO → TIER, 再 min_qty DESC → effective_from DESC → id ASC
③ biz_price_tier customer is null  同 ②
④ biz_material.standard_price   非 NULL 才给价（NULL 继续往下走 → NONE）
⑤ 全未命中 → {price:null, source:'NONE', source_id:null}（HTTP 200，不是 404）
```

为什么必须补二级/三级键：`uk_biz_cust_price` 是 `(customer, material, min_qty, effective_from)`，
**同 min_qty 不同生效日**的两条价可以同时有效（旧价没失效、新价已生效）；
`biz_price_tier` 的唯一键含 `tier_type`，同区间的 `TIER` 与 `PROMO` 是**合法并存**。
只按卡面的「min_qty 最大」排会把同键并列交给物理行序（VACUUM/UPDATE 后可能变），
同一张单今天算 10 元、明天算 12 元。`id ASC` 是最后兜底（防历史脏数据绕过唯一索引）。

## 3. 改动清单

| 文件 | 说明 |
|------|------|
| `backend/src/business/erp/price_dto.rs`（新） | 入参校验 + 出参契约 + source 域值（8 个单测） |
| `backend/src/business/erp/price_model.rs`（新） | 三级粗筛 SQL + **纯函数**窗口判定/全序选优/五级链（23 个单测） |
| `backend/src/business/erp/price_handler.rs`（新） | 编排（存在性 → 短路取价）+ `#[nik::permission]` 入口（3 个单测） |
| `backend/src/business/erp/mod.rs` | 注册三个 mod |
| `backend/migrations/business/V0307__price_resolve_permission.{up,down}.sql` | 权限码菜单承载行（1 个隐藏页面节点，幂等 + 自检） |
| `scripts/verify/so33_instance.sh` | 隔离实例起停（库 cloudpivot_so33 / 端口 18239） |
| `scripts/verify/so33_price_fixture.sql` | e2e 夹具（2 客户 / 4 物料 / 6 协议价 / 3 档位） |
| `scripts/verify/so33_price_resolve_e2e.py` | 22 条断言（真实 token、真实库） |
| `scripts/verify/so33_price_curl_evidence.sh` | 原样 curl 留证 |

**过滤为什么写在 Rust 而不是 SQL**：有效期边界（`effective_to` 含）与档位右开（`qty < max_qty`）
若写在 SQL，只能靠 `ERP_DB_URL` 开关的真库用例验证（本仓不设该变量时静默 SKIP = 假绿）；
写进纯函数后 `cargo test --lib` 必跑边界。SQL 只做索引前缀粗筛（客户/物料/ACTIVE/未软删），
窗口条件**只存在于一处**，不会两份实现分叉。

## 4. 验收证据

| 证据 | 文件 | 结果 |
|------|------|------|
| 全量单测 | `cargo_test_lib.txt` | **2922 passed / 0 failed**（SO-32 后是 2888，本卡 +34） |
| 本卡单测 | `cargo_test_price_only.txt` | 34 个 price_* 用例全绿 |
| 真实 HTTP 断言 | `e2e_curl.txt` | **22 条断言 FAIL 0 / ALL_PASS** |
| 原样 curl 现场 | `curl_transcript.txt` | 5 级 + 边界 + 失败路径的原始响应体 |
| 迁移幂等 | `migration_idempotency.txt` | 二次 up no-op / down 删除 1 行 / 二次 down no-op / 再 up 恢复；`applied_via` 非法值 0 行；`307|versioned|executed|t` |
| 权限码闸 | `perm_menu_gate.txt` | `missing_in_menu = 0`，闸 PASS |

curl 实测的 5 种场景（原文见 `curl_transcript.txt`）：

| 场景 | 请求 | 响应 data |
|------|------|-----------|
| ① 协议价（多条取 min_qty 最大） | C1×M1 qty=150 | `{"price":"9.0000","source":"AGREED","source_id":"…0102"}` |
| ② 客户专属阶梯（PROMO 压同段 TIER） | C1×M2 qty=50 | `{"price":"6.5000","source":"TIER_CUSTOMER","source_id":"…0202"}` |
| ③ 通用阶梯 | C2×M2 qty=50 | `{"price":"8.0000","source":"TIER_COMMON","source_id":"…0203"}` |
| ④ 物料标准价 | C2×M3 qty=10 | `{"price":"12.5000","source":"STANDARD","source_id":"<物料 id>"}` |
| ⑤ 全未命中 | C2×M4 qty=10 | `{"price":null,"source":"NONE","source_id":null}` |

边界（同一份证据里）：`effective_to` 当天生效 → 次日降级；未到 `effective_from` 不用；
同 min_qty=50 取最近生效（9.8000 → 9.6000）；`qty == max_qty` 不命中档位（右开）；
INACTIVE 的 1.0000 从未被命中；同参数连打 10 次响应体完全一致（确定性）。

## 5. 复现

```bash
cd /opt/cloudpivot
su - postgres -c "psql -c 'CREATE DATABASE cloudpivot_so33 OWNER alun TEMPLATE cloudpivot_so32'"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so33 -v ON_ERROR_STOP=1 \
    -f scripts/verify/so33_price_fixture.sql
export PATH=/root/.cargo/bin:$PATH && (cd backend && cargo build)
bash scripts/verify/so33_instance.sh up
python3 scripts/verify/so33_price_resolve_e2e.py      # 22 条断言
bash scripts/verify/so33_price_curl_evidence.sh        # 原样 curl
bash scripts/verify/so33_instance.sh down
```

注意：夹具里有一句 `UPDATE sys_user SET must_change_password=FALSE WHERE username='admin'`
—— 隔离库的 admin 处于「首次登录必须改密」状态，`session_guard` 会把所有接口挡成 403。
只在隔离库做，生产绝不这么做。

## 6. 明确不在本卡范围

* 价格 CRUD 与价格页面（SO-32 遗留）：本卡只做**读取口径**，V0307 的菜单节点 `path`/`component`
  留 NULL 且 `is_visible=FALSE`（`system::menu::tests::n08` 会因前端无路由判红）；
* 下单时的**价格快照**：销售订单行的 `price` 列（SO-04）负责冻结成交价，本接口只回答
  「此刻按规则应该是多少」；
* 授权：V0307 不写 `sys_role_menu`（授谁是管理决策），现状仅 SUPER_ADMIN 可调。
