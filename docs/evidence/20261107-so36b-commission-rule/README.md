# SO-36b 佣金规则 CRUD 接口 —— 证据包

卡：`t_d30bcd52`（SO-35 台账登记的「佣金规则 CRUD」，SO-37 前端依赖）。
库：`cloudpivot_so36`（SO-36 的隔离库，本卡重放 `so36_fixture.sql` 拿干净基线），实例端口 `18243`。
代码：`backend/src/business/erp/commission_rule_{dto,model,handler}.rs`（四件套 + `mod.rs` 注册）。

## 一、交付内容

| 方法 | 路径 | 权限码 | 落点 |
|---|---|---|---|
| GET | `/api/business/erp/commission-rule/page` | `business:erp:commission-rule:list` | `commission_rule_handler::commission_rule_page_api` |
| POST | `/api/business/erp/commission-rule` | `business:erp:commission-rule:create` | `...::commission_rule_create_api` |
| PUT | `/api/business/erp/commission-rule/{id}` | `business:erp:commission-rule:update` | `...::commission_rule_update_api` |
| DELETE | `/api/business/erp/commission-rule/{id}` | `business:erp:commission-rule:delete` | `...::commission_rule_delete_api` |

权限码逐字复用 `V0310` 的 `sys_menu.auth_code`，**没有新增迁移**（表在 V0309、码在 V0310，本卡纯接口层）。
`permission_audit_tests::ROUTE_LEVEL_ONLY` 里 4 条 `commission-rule:*` 例外**已删除**（落地即删，N18 持续校验）。

## 二、接口形状（与 SO-37 前端逐字对齐）

出参（`RuleRes`）：`id / rule_name / scope_type / scope_id / scope_name / calc_type / rate /
min_amount / max_amount / effective_from / effective_to / status / remark / created_at / updated_at`

* 金额与比率是 `NUMERIC` → JSON **字符串**（`"4.0000"` / `"60.0000"`），前端只展示不自算；
* `scope_name` 是派生列：`SALESPERSON` → `sys_user` 姓名（`COALESCE(NULLIF(real_name,''), username)`）、
  `MATERIAL_CATEGORY` → `biz_material.material_name`、`ALL` → `null`。两个 JOIN 都带
  `scope_type` 条件（互斥命中），且**不过滤** `deleted_at`（对象被删也还读得到当时的名字）；
* `effective_to = null` = 长期有效；`max_amount = null` = 不封顶。

入参兼容：前端 PUT **发全量**（与整体覆盖等价），后端按**部分更新**实现
（`{"status":"INACTIVE"}` 就能停用）。三态字段（`scope_id` / `max_amount` / `effective_to`）
用 `double_option` 区分「字段缺失（不改）」与「显式 `null`（清空）」。

## 三、口径决策

1. **停用 ≠ 删除**：停用 = `PUT {"status":"INACTIVE"}`，规则退出计提匹配
   （`commission_model::load_rules` 的 `WHERE status='ACTIVE'`），历史明细仍能用 `rule_id` 查到它；
   软删（`DELETE`）只用于「录错了」——**被存活结算单明细引用过的规则一律 409 顶回**，
   逼调用方改停用（否则历史明细的 `rule_id` 指向一条被删的配置，审计断链）。
2. **`MATERIAL_CATEGORY` 的 `scope_id` 本期 = `biz_material.id`**（基础版无 `biz_material_category`，
   见 `commission_handler.rs` 头注释）。接口按「物料存在且未软删」查 `biz_material`；
   查的对象不存在 → 400 指名（不静默放过：写错表的规则**永远匹配不上**，而且不报错）。
3. **入口先拒库层约束**：`ALL ⇔ scope_id IS NULL`、`PERCENT 0–100`、`FIXED ≥ 0`、
   `min_amount ≥ 0`、`max_amount ≥ min_amount`、`effective_to ≥ effective_from`、`status ∈ {ACTIVE,INACTIVE}`。
   另外挡住「列宽装不下」（`rate NUMERIC(8,4)` ⇒ ≤ 4 位小数且 < 10000；金额 `NUMERIC(18,4)`）——
   超了库会**静默四舍五入**，用户填的规则和实际计佣的规则就不是同一条了。
4. **`effective_from` 缺省 = 今天（北京时间）**，与 DDL `DEFAULT CURRENT_DATE` 同口径；
   日期参数形状先卡死 10 位 `YYYY-MM-DD`（chrono 的 `%d` 会接受 `2026-11-7`，接口契约不放它进来）。
5. **规则不参与数据权限**：`biz_commission_rule` 是配置表（无归属人 / 部门列），
   分页走 `model_base::page_of → scope::paginate_unscoped`；
   **不要**把它加进 `scope::SCOPED_TABLES`（加了分页入口会直接报错），单测钉住。

## 四、修复的线上 500（SO-37 实测报障）

SO-37 前端点「新建规则」拿到 `HTTP 500 / code 10004`，服务端日志：

```
error=query failed: INSERT INTO biz_commission_rule (...) VALUES (... $9::date ...):
error serializing parameter 8
```

**根因**：`effective_from` 占位符写成 `?::date`，驱动按**服务端推断的参数类型**编码
（`vendor/nik-db-driver-postgres/src/connection.rs::cached_stmt` 走 `PREPARE`），
PG 把 `$9` 推断成 `date`，而参数值是 `Value::Text`（文本只能编码进 text 家族）⇒
tokio-postgres 的 `ToSql` 拒绝（0 基索引 8 = 第 9 个参数 = `effective_from`）。

**修法**：日期参数改 `?::text::date`（先转 text 再转 date），参数类型回落 text。
这是本仓既有教训（`period.rs` 关闭期间那段注释写过同一条），本卡把规矩**钉进单测**：
`commission_rule_model::tests::date_and_numeric_params_are_cast_through_text`
——扫描三条 SQL，禁止裸 `?::date` / `?::numeric`，并核对占位符个数（14 / 13 / 2）与 `vec!` 参数个数一致。

同批还修掉：`remark` 空值落 `''` 而不是 `NULL`（SQL 改 `NULLIF(?::text,'')`，与出参契约一致）。

## 五、验证（全部真跑）

| 项 | 结果 | 证据 |
|---|---|---|
| `cargo test --lib` | **3030 passed / 0 failed**（新增 31 条：dto 17 / model 9 / handler 8 里含 5 条语义回归） | `full_test.txt` |
| `cargo clippy --all-targets -- -D warnings` | 干净 | `gates.txt` |
| `cargo fmt --check`（本卡 4 个文件） | 干净 | `gates.txt` |
| 真实 token curl | **63 条断言 ALL CHECKS PASSED** | `curl_transcript.txt` |
| 落库态 | 7 行（3 夹具 + 4 本卡），`deleted_at` 只有「录错了」那条为 `t` | `db_state.txt` |

curl 覆盖（`scripts/verify/so36b_curl_evidence.sh`）：

```
A 语义回归（真库真计提，证明规则改动确实影响计佣）
  A1 2026-11 计提 → SP1 基数 400 / 佣金 80（销售员 20% 胜出）
  A2 AR-A 已收 400→600，2027-01 计提 → delta 200 / 佣金 40，命中「SO36 销售员甲 20%」
  A3 PUT 停用该规则（只发 status）→ 其余 10 个业务字段逐字不变
  A4 AR-A 已收 600→800，2027-02 计提 → 16 = 物料一 10%(12) + 全局 5%(4)
     ⇒ 停用后回落到物料 / 全局规则，金额随之变化
  A5 PUT 恢复 ACTIVE（停用可逆）
B CRUD
  B1 新建 4 条（全局 / 物料 / 销售员 / 固定额）：4 位小数字符串、scope_name 三种形态
  B2 分页与筛选：keyword（规则名 + 范围名）/ status / scope_type / calc_type / 排序首键 status
  B3 PUT 部分更新（只发 rate → 其余不变）；显式 null 清空 max_amount / effective_to
  B4 DELETE 未被引用 → 200、列表消失、重复删 → 404
  B5 DELETE 被明细引用 → 409（psql 核对引用数 > 0 后打的）
C 负路径：scope 不一致 / 物料不存在 / PERCENT=120 / 生效期倒挂 / 封顶<起算 / status 乱码 /
          rate 缺失 / 筛选乱码 → 400；四个接口无 token → 401
```

## 六、单测清单（31 条，`#[cfg(test)]` 内联在各文件）

* `commission_rule_dto`（17）：域值与 V0309 CHECK 对齐、scope 三态、PERCENT 上界、FIXED 非负与列宽、
  金额 min/max 关系、生效期端点、日期形状、状态归一化、spec 交叉校验、默认值填充、rate 必填、
  部分更新合并（含清空）、`double_option` 语义、分页入参归一化。
* `commission_rule_model`（9）：列清单覆盖 V0309 的 14 个读出列、`SCOPE-EXEMPT`（不在 SCOPED_TABLES）、
  排序全序、`scope_name` JOIN 带 `scope_type` 守卫（且用 `material_name` 不是 `name`）、
  可空参数 `NULLIF` 形态、**日期/金额参数必须 `?::text::` 转型 + 占位符个数**、`to_spec` 保真。
* `commission_rule_handler`（含 5 条语义回归）：
  `rule_update_changes_pick_rule_outcome`（改比率 / 改范围 / 缩短生效期 / 停用 四条路径下
  `pick_rule` + `commission_of` 的结果变化）、`crud_does_not_break_rule_priority_order`
  （销售员 > 物料 > 全局，时间不是优先级）、`fixed_rule_amounts_flow_into_commission_of`（起算门槛与封顶）、
  `stopping_a_rule_is_a_status_change_not_a_delete`、`switching_scope_type_requires_scope_id`、
  出参字段名逐个核对（前端 `undefined` 的防线）。

## 七、复现

```bash
# 1) 二进制含本卡代码
cd /opt/cloudpivot/backend && cargo build

# 2) 隔离实例（库 cloudpivot_so36，端口 18243；SO-36 的脚本，本卡复用）
cd /opt/cloudpivot && bash scripts/verify/so36_instance.sh up

# 3) 证据（脚本自己重放 so36_fixture.sql 拿干净基线，可反复执行）
bash scripts/verify/so36b_curl_evidence.sh | tee /tmp/so36b_curl.txt

# 4) 收工
bash scripts/verify/so36_instance.sh down
```

## 八、残留

* 规则「同一范围同一期间多条重叠」是**允许**的（`pick_rule` 有全序：生效日新 → 创建新 → id 小），
  接口不做重叠告警 —— 属产品侧「规则冲突提示」需求，未在本卡。
* `MATERIAL_CATEGORY` 接分类表后需一次数据迁移把 `scope_id` 从 `biz_material.id` 换成类别 id（沿用 SO-36 的决策）。
* 无 `GET /{id}` 详情接口：列表行已含全部字段，前端编辑直接用列表行（卡面只列 4 个接口）。
