# COST-B2-4【成本】费用台账后端 —— 证据

卡面验收（3 条）与落地位置：

| 卡面验收 | 落地 | 证据 |
|---|---|---|
| CRUD | C（`POST`）/ R（`GET` 分页）/ D（`DELETE` 软删）—— **设计 §5.2 只给这 3 个接口，没有 PUT**（见「已知偏差 ①」） | `03_http_smoke.out` 步骤 2/3/4/9/10、`02_real_db_tests.out` 用例 1 |
| 科目不在 `4101.*` → 拒绝 | `dto::normalize_debit_account` + `handler::ensure_account` → `400 + 66060`；库层 `ck_biz_cost_oh_debit` 同口径兜底 | `02_real_db_tests.out` 用例 2/3、`03_http_smoke.out` 步骤 5 |
| 金额必须 > 0 | `dto::ensure_amount` → `400 + 66061`；库层 `ck_biz_cost_oh_amount` 同口径兜底 | `02_real_db_tests.out` 用例 4、`03_http_smoke.out` 步骤 6 |

## 交付物

```
backend/src/business/erp/cost_overhead_dto.rs        契约层（值域 / 校验 / 出参 / 错误翻译）  12 条单测
backend/src/business/erp/cost_overhead_model.rs      数据层（1 张表 + 5 处 join + 分页 + 写） 10 条单测
backend/src/business/erp/cost_overhead_handler.rs    接口层（3 个 HTTP 入口 + 业务编排）      8 条单测
backend/src/business/erp/cost_overhead_db_tests.rs   真库用例（ERP_DB_URL 开关）              6 条
backend/src/business/erp/mod.rs                      模块注册（+3 个 pub mod + 1 个 #[cfg(test)]）
backend/src/shared/error_code.rs                     新增 66060 / 66061 + 号段表说明
```

**无新增迁移**：表 `biz_cost_overhead` / 索引 / 5 条 CHECK 全在 B1-2 的 `V0371`，
权限菜单行在 B1-7 的 `V0378`（`business:erp:cost-overhead:{list,create,delete}` 三个码已种好，
本卡只是把它们的承载接口补上）。⇒ 卡面「迁移幂等 + 配套 .down.sql」对本卡 **N/A**，
**未碰任何已应用迁移**。

## 一、纯逻辑单测（30 条，见 `01_unit_tests.out`）

```
cargo test --lib cost_overhead_dto      12 passed
cargo test --lib cost_overhead_model    10 passed
cargo test --lib cost_overhead_handler   8 passed
cargo test --lib error_code             11 passed   ← 66xxx 码值稳定性（含新增 66060 / 66061）
```

覆盖到的判定（每条都是「入口放行 / 库层拒绝」的分界线）：

- `4101.*` 判定与库 CHECK 等价：`4101.01 / 4101.01.02` 合法；`4101` / `41010.1` / `4101.` /
  `4101.abc` / `5602.01` / 带前后空格 一律拒；
- 金额：`0` / `0.0000` / `-1` / `-0.01` → 66061；`12.34567`（5 位小数）→ 66061 **不静默四舍五入**；
  `100000000000000` 超 `NUMERIC(18,4)` → 66061；`4000.0000`（尾零）正常放行；
- 费用项目：大小写归一 + 7 值白名单，报错把 7 个编码连中文名一起列出；
- 期间 / UUID / 备注长度：缺字段的报文**点名是哪个字段**；
- 库错误翻译：`ck_biz_cost_oh_amount → 66061`、`ck_biz_cost_oh_debit → 66060`、
  其它约束 → 400 点名约束名、外键 → 400（对象被并发删掉）；
- 登记纪律：`biz_cost_overhead` **不在** `SCOPED_TABLES`（断言死）；
- SQL 形状：7 处 join 全在、`*_id` 都带 `*_name`、建单参数与占位符**逐位对齐**、
  软删带 `deleted_at IS NULL` 闸。

## 二、真库用例（6/6，见 `02_real_db_tests.out`）—— 库 = `mig_replay_b24`（空库全链重放 301/301）

```
test create_list_delete_flow_end_to_end ............... ok   ← CRUD + 名字全 join 出来 + 金额 4000.0000
test debit_account_outside_4101_is_rejected ........... ok   ← 5602.01/6401/1405/4101 全 400+66060，零落库
test debit_account_must_exist_be_leaf_and_enabled ..... ok   ← 不存在/非末级/已停用 三条；末级启用放行
test amount_must_be_positive_at_entry_and_in_the_db ... ok   ← 入口 66061 + 绕过直插撞 CHECK → 同码
test cost_object_must_exist_and_be_open ............... ok   ← 400/66002 · 409/66057 · 在制回显对象号
test deleting_a_collected_row_is_refused .............. ok   ← 已归集 409 点名期间，归集行删掉后可删
test result: ok. 6 passed; 0 failed
```

跑完夹具零残留（`cost_overhead=0 / cost_collect=0 / 夹具科目=0 / 夹具对象=0 / 夹具物料=0 / 夹具仓库=0`）。

## 三、HTTP 冒烟（真实例 `127.0.0.1:18027`，空库首启全链迁移，admin 真登录，见 `03_http_smoke.out`）

```
2) POST /business/erp/cost-overhead   code=0 amount=4000.0000
     debit=4101.02/制造费用-水电费  credit=1002/银行存款  item=UTILITY/水电费
     dept=总部  object=None（车间费用池行）  source=MANUAL
3) POST（借方留空）                   code=0 debit=4101.09/制造费用-其他 amount=120.5000   ← 缺省值生效
4) GET  列表（期间+费用项目+关键字）   code=0 total=1 首行=2026-11/4101.02/4000.0000
5) POST 借方 5602.01                  HTTP 400 code=66060「借方科目「5602.01」不是制造费用科目…」
6) POST 金额 0                        HTTP 400 code=66061「金额必须大于 0（收到 0）」
7) POST 费用项目 TRAVEL               HTTP 400 code=10001（报文列出 7 个合法值 + 中文名）
8) POST 成本对象不存在                HTTP 400 code=66002
9) DELETE ×2                          code=0 / code=0
10) DELETE 同一行第二次               HTTP 404 code=10002
11) GET 列表（删完）                  code=0 total=0
12) 无 token：GET/POST/DELETE 全 401；未知路径 404（没被通配路由吞掉）
```

## 四、提交前门禁（见 `04_gates.out`）

```
cargo fmt -- --check                        clean
cargo clippy --all-targets -- -D warnings   EXIT=0（0 warning / 0 error）
cargo test（全量）                           33 个测试目标 / 4664 passed / 0 failed / EXIT=0
空库全链迁移重放                            301/301 PASS（mig_replay_b24）
```

## 五、关键设计点（为什么这么写）

1. **科目合法性除了前缀还查账套现状**（比设计文字多做一步，但不多做「重新设计」）。
   设计只写「只允许 `4101.*`」，本卡另外拦了三种**到结算才炸**的输入：
   账套里没有的编码（`4101.99`）、非末级科目（凭证闸 `non_leaf_codes` 只允许末级记账）、
   已停用科目（`status=0`）。三者同一条码 `66060`（动作都是「换科目」），报文点名原因。
2. **金额不做静默四舍五入**。录 `12.34567` 直接 400，而不是悄悄存成 `12.3457` ——
   改数会让账上金额与单据对不上，而这笔钱要进产品成本（与设计 §4.5.2 硬规则 1「不猜」同源，
   也与工时台账「超精度直接 400」同一条纪律）。
3. **删除前先问「有没有被归集」**。本表无状态列（设计如此：录错就删），但若这一行已被某期
   `calc` 归集进 `biz_cost_collect`（`source_type='OVERHEAD_TICKET'`），删掉就等于让已归集的钱
   失去来源 —— 账上有钱、单据没了、界面没有可点的按钮，只能改库恢复（AGENTS 约定 11 禁止的形态）。
   所以删除前查一次归集，命中就 409 并告诉用户走「作废/重算该期间」。
4. **费用项目名走字典**。`expense_item` 是**字典项**（V0379 种了 7 项），列表出参 join
   `sys_dict_item` 拿实时 label，字典行被删时回落到内置 7 项文案（不会显示裸 `UTILITY`）。
5. **参数解析复用 `cost_labor_dto`**（期间格式 / UUID / 文本清洗）。再抄一份的唯一后果是
   「两套校验漂移」——某天有人只改了一边的期间正则，同一张月结单里工时能录 `2026-13`、费用不能。
6. **列表显式 unscoped**。设计 D-18 点名的登记表只有成本对象 / 工时台账 / 成本对象项三张；
   费用台账是**财务**录的池行，按部门裁剪会让集团财务看不见自己录的、分摊时反而找不到数据。
   分页仍走唯一入口 `paginate_unscoped`（不自拼 LIMIT/OFFSET），并在 model 注释里写清
   「若将来要按车间裁剪，需同时改 `SCOPED_TABLES` + `guard.rs` 计数断言 + 换 `paginate`」。
7. **`dept_name` 存快照、展示取实时名**（AGENTS 约定 13：主数据关联 join 实时名，
   审计快照单独存列）。成本对象号、科目名同样 join 实时取，一处 SQL 拿全（无 N+1）。

## 六、已知偏差 / 需知会下游

1. **没有 PUT（CRUD 的 U 缺失）** —— 设计证据三条一致：§5.2 只列 `GET/POST/DELETE`、
   §5.3 只给 `list|create|delete` 三个码、V0378 菜单种子只种「新增费用/删除费用」两个按钮。
   后者的承载行在**已应用**迁移里，新开 `cost-overhead:update` 会让 `perm_menu_gate` 直接红。
   ⇒ 按设计交付 **C/R/D**：录错了删掉重录（软删，行留库里可追溯）。
   **若业务坚持要编辑**：需设计增量 + 一条新迁移补权限码与菜单按钮，单独立卡。
2. **费用台账导入（xlsx）未实现** —— 设计 §5.4 明写「费用台账导入**二期**再开（一期表单够用）」，
   `source_type='IMPORT'` 的取值本卡只登记在值域里，没有写入方。
3. **期间结账状态未拦** —— 向已结账期间补录费用，本卡不拒绝（与工时台账同口径）：
   结账后能否动台账属「财务期间口径」，由结算卡的期间闸统一处理，不在录入侧各写一套。
4. **未登记数据权限** —— 见「关键设计点 6」，按设计 D-18 落地，若要求按车间裁剪是独立裁决。
5. **无迁移** —— 表 / 索引 / CHECK（V0371）与权限菜单（V0378）都是上游卡已交付的，
   本卡零迁移；卡面「迁移幂等 + down.sql」对本卡 N/A。
6. **遗留库** —— 本机 `mig_replay_b24`（真库用例用，已清干净，夹具零残留）保留供后续卡验证；
   冒烟库 `costb24_http` 已由 `00_up.sh down` 删除。`mig_replay_costb18` 是更早卡遗留的
   **未完整迁移**库（`4101` 还是末级、无 `4101.*` 子科目、字典 0 行），**不能**用于成本类真库用例。

## 复跑

```bash
# 纯逻辑
cd backend && cargo test --lib cost_overhead_dto && cargo test --lib cost_overhead_model \
  && cargo test --lib cost_overhead_handler && cargo test --lib error_code

# 真库（需要可写的 PG；本机 alun/alun；库必须跑过全链迁移）
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/mig_replay_b24 \
  cargo test --lib cost_overhead_db -- --test-threads=1

# 空库全链迁移重放（301/301）
KEEP_DB=1 bash scripts/verify/mig_replay_fresh.sh b24

# HTTP 冒烟（起隔离实例 → 跑脚本 → 拆）
bash docs/evidence/2026-11-11-cost-b2-4-cost-overhead/00_up.sh up
BASE=http://127.0.0.1:18027 DB=costb24_http \
  bash docs/evidence/2026-11-11-cost-b2-4-cost-overhead/03_http_smoke.sh
bash docs/evidence/2026-11-11-cost-b2-4-cost-overhead/00_up.sh down
```
