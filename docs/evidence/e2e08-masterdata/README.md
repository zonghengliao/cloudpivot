# E2E-08 主数据域演练证据（客户 / 供应商 / 物料 / 单位 / 仓库 / 客户价格 + 自动编码）

- 任务：`t_be02cbc5`（E2E-08）
- 环境：**145 测试机** · `cloudpivot_dev` 库 · 真后端 `http://127.0.0.1:8033`（经 `/api`）· 演练时实例 commit **8005a872**
- 时间：2026-11-09 12:00 ~ 12:07 CST
- 脚本：`scripts/verify/e2e08_{reset,master_common,master_a,master_b,master_c,db_evidence,run_all,cleanup,deploy_145}.sh`（一把跑：`bash scripts/verify/e2e08_run_all.sh`）
- 断言：**364 PASS / 0 FAIL**（A 221 + B 71 + C 72），另有前端单测 **86/86 PASS**
- 数据纪律：只造 `E2E08` 前缀数据；不 DROP / TRUNCATE；不动他人演练数据；演练后 `e2e08_cleanup.sh` 只清自己的前缀

## 一、演练范围

| # | 页面 | 本卡覆盖 |
|---|---|---|
| ① | 客户档案 | 建（自动/手工/三种留空形态）· 查（code/name/status 过滤）· 改（名称/额度/账期/冻结）· 删（软删 + 编码复用）· 非法状态拦截 |
| ② | 供应商档案 | 建 · 详情 · 改（账期/额度/联系人）· 停用 + 按 status 过滤 · 删 · 库内 deleted_at |
| ③ | 物料管理 | 建（规格/单位/默认仓/税率/安全库存/**一物一码**）· 改 · **序列号批量生成**（开关关闭→400、打开→3 个号）· 序列号分页 · 删 |
| ④ | 计量单位 | 基础单位 + 折算单位（base_unit_id/rate=12）· **不自动编码**（缺 code 被拒、无 next-code 接口）· 重复码 409 · **被物料引用不可删（409）** |
| ⑤ | 仓库管理 | 父仓 + 子仓（parent_id/level/path）· 树接口 · 改名 · 有下级不可删（409）· 库位不可删 · **被物料默认仓引用不可删（本次新增闸）** |
| ⑥ | 客户价格 | 协议价 CRUD + 唯一键 409 + price/日期边界 · 阶梯价（区间、左闭右开边界、与协议价层级）· **促销价生效期** · 通用档位 · 五级取价优先级 · 清空回落 NONE |
| — | 自动编码（重点） | `SUP/CUS/MAT/WH/AC/ET` 六类 × 16 条路径矩阵：预览只读不占号 / null·空串·纯空白三态取号 / 连续 +1 / 手工保留原值 / 重复 409+66001 点名编码 / 非法字符 400 / 超长 400 / 库内一致 / 列表可检索 / 编辑不改码 / **预览号当手工码提交（「生成编码」按钮闭环）** |
| — | 编码策略开关 | `erp.master_data.allow_manual_code=false`：策略接口即时生效、手工码 400+66054、留空仍取号、恢复 true 后手工可用 |
| — | 历史回归 | CODE-01~09 自动编码 30 用例（本卡扩到六类 × 16 条路径 = **96 条**编码断言）· **AUD-01 附件 uploader_name 快照**（上传→改名→快照不回改→列表仍显示旧名） |

## 二、结果总览（节选，全量见 `asserts.log`）

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| A-③ | `POST /business/erp/supplier` 不带编码 | 200 | `supplier_code=SUP-0037`，`biz_seq.SUP` 同步 +1 | 自动取号 ✅ |
| A-② | `GET /supplier/next-code` ×2 | 200 | `current_no` 不变，两次同号 | 预览不占号 ✅ |
| A-⑥ | 手工编码 `E2E08-SUP-M1` | 200 | 原值落库，号段不推进 | 保留原值 ✅ |
| A-⑧ | 同编码再建 | 409 / 66001 | 无新行 | 文案点名编码 ✅ |
| A-⑨ | 编码含空格 | 400 / 10001 | — | 字符集拦截 ✅ |
| A-⑬ | `PUT` 编辑（不带编码） | 200 | 编码不变（AC/ET 需带名称，否则 400 名称必填） | ✅ |
| A-⑭ | **「生成编码」按钮闭环**：`GET next-code` 取号 → 保存时把该号当手工码提交 | 200 | 落库编码 == 预览号，`current_no` 不推进 | 按钮预览+填充可用 ✅ |
| B-① | 物料 `serial_required=true` | 200 | `biz_material.serial_required=t` | ✅ |
| B-② | 删被物料引用的单位 | **409**「还有 1 个物料在用该单位」 | 单位仍在 | 引用闸 ✅ |
| B-③ | 关一物一码后批量生成序列号 | 400「未开启一物一码（serial_required = FALSE）」 | 0 行 | 开关生效 ✅ |
| B-④ | 开一物一码后生成 3 个 | 200 `count=3` | `biz_material_serial` 3 行、serial_no 3 个互异、状态 IN_STOCK | ✅ |
| B-⑤ | 删被物料设为默认仓的子仓 | **409**「还有 1 个物料把该仓库设为默认仓…」（本次修复） | 仓库仍存活，物料默认仓未悬空 | ✅ |
| B-⑥ | 有下级时删父仓 | 409「仓库还有 1 个下级」 | 父子都存活 | ✅ |
| B-⑦ | 客户冻结 → 列表 `status=FROZEN` | 200 / total=1 | `status=FROZEN` | ✅ |
| B-⑧ | 客户软删后同编码再建 | 200 | 新行存活，旧行 `deleted_at` | 编码可复用 ✅ |
| C-① | 协议价 100（2027 全年） | 200 | `biz_customer_price.price=100.0000` | ✅ |
| C-② | `GET /price/resolve` qty=5 | 200 | `price=100.0000 source=AGREED` | 协议价最高优先 ✅ |
| C-③ | 同客户/物料/起订量/生效日再建 | 409「同一组键只能一条」 | — | 唯一键 ✅ |
| C-④ | 改价 120 | 200 | 取价 120 | ✅ |
| C-⑤ | 停用协议价，qty=8 | 200 | `95.0000 / TIER_CUSTOMER` | 层级回落 ✅ |
| C-⑥ | qty=10（区间左闭右开） | 200 | `90.0000` | 边界正确 ✅ |
| C-⑦ | 促销价 80（2027-06） | 200 | 06-15→`80.0000`（命中 PROMO 档，`source=TIER_CUSTOMER`）；07-15→`95.0000` | 生效期生效 ✅ |
| C-⑧ | 历史促销 70（2026-01） | 200 | 2026-01-15→70；2027-07→95 | 期间隔离 ✅ |
| C-⑨ | 通用档 85（customer_id=null） | 200 | qty=60→85 TIER_COMMON；qty=8→95 客户档 | 层级正确 ✅ |
| C-⑩ | 删光价格 | 200 | 存活价格行 0，`resolve` → `null / NONE` | ✅ |
| C-⑪ | 参数 `allow_manual_code=false` | 200 | 策略接口立刻 false | 热生效 ✅ |
| C-⑫ | 开关关闭 + 手工码 | 400 / 66054 | 无新行、不占号 | ✅ |
| C-⑬ | 开关关闭 + 留空 | 200 | `SUP-00xx` 自动号 | ✅ |
| C-⑭ | 恢复 true | 200 | 手工码 200 且原值落库 | ✅ |
| C-⑮ | AUD-01：演练用户上传附件 | 200 | `biz_doc_attachment.uploader_name='E2E08 审计员'` | 快照 ✅ |
| C-⑯ | 管理员把该用户改名 | 200 | `sys_user.real_name` 变新名；`uploader_name` **仍是旧名** | 快照不回改 ✅ |

## 三、勾稽验证（13 组，见 `db-evidence.txt`）

| # | 勾稽项 | 结果 |
|---|---|---|
| 1 | 六类接口返回编码 == 库内列（`biz_supplier.supplier_code` 等） | 6/6 一致 |
| 2 | 自动取号次数 == `biz_seq.current_no` 增量（每类 3 次自动 +1） | 6/6 一致，手工/预览 0 增量 |
| 3 | 预览两次同号 且 `current_no` 不变 | 6/6 一致 |
| 4 | 六类存活编码零重复（全库扫描） | 0 / 0 / 0 / 0 / 0 / 0 |
| 5 | 取价结果 == 价格表当前生效行（95/90/92/80/70/85） | 6/6 一致 |
| 6 | 价格全删后存活价格行 == 0 且取价 NONE | 一致 |
| 7 | 阶梯区间重叠对数（本卡物料 / 全库） | 0 / 0 |
| 8 | `serial_required` 与 `biz_material_serial` 行数（3 个号） | 3 == 3 |
| 9 | 序列号状态分布 | 全 IN_STOCK |
| 10 | 附件 `uploader_name` 非空且非 UUID，且与上传时 `real_name` 一致 | 一致（改名后不变） |
| 11 | 悬空引用：存活物料默认仓指向已删仓库 | **0**（修复后） |
| 12 | 编码策略参数演练后归位 | `allow_manual_code=true` |
| 13 | 物料软删后序列号保留（审计口径不级联） | 3 行保留 |

## 四、问题清单

| # | 问题 | 级别 | 复现步骤 | 影响 | 建议 |
|---|---|---|---|---|---|
| 1 | **删仓库不校验「物料默认仓」引用**：把物料 `default_warehouse_id` 指向的仓库 DELETE 掉返回 200，物料仍指向已软删行 | **P3（本卡已修）** | ① 建仓 W → ② 建物料默认仓=W → ③ `DELETE /business/erp/warehouse/{W}` | 入库单/调拨单默认带出的仓库是已删行（页面空白、逻辑悬空），只能改物料默认仓恢复；与「单位被引用不可删」口径不一致 | 删除前统计 `biz_material.default_warehouse_id`，>0 则 409 并给出两条出路（调整默认仓 / 改为停用）。**已实现**（commit `8005a872`） |
| 2 | **阶梯价区间不重叠未拦截**：同物料/客户/类型/生效期内，`min_qty 5~20` 与 `1~10`、`10~50` 可同时 ACTIVE | P3 | `POST /business/erp/price/tier` 建两个 qty 区间相交的 TIER 档 → 均 200 | 取价虽确定（`min_qty DESC` → 命中 92），但配置层允许自相矛盾的价格表，业务方无法自证「哪档才算」；与卡面「区间不重叠」要求不符 | 建/改档时加区间相交校验（同物料/客户/类型 + 生效期相交 + qty 区间相交 ⇒ 409），或产品明确「允许重叠、取 min_qty 最大者」并写进文档。需产品拍板，本卡未改（涉及 2 文件 + 既有用例） |
| 3 | **客户编码可被 `PUT` 改写**：`{"code":"E2E08-CUS-CHG"}` 返回 200 且真的改了 | P3 | `PUT /business/erp/customer/{id}` 带新 code | 六类同族口径不一致：资产分类改码 → 400「编码不可修改」；供应商/物料/仓库压根不接受该字段；费用类型归一；只有客户能改。前端编辑态只读→界面不可达，但接口可绕过，历史审计依赖编码时会串 | 客户 `update_customer` 加「非空且不等于现值 ⇒ 400 编码不可修改」；或产品确认「客户编码允许变更」并同步其余五类 |
| 4 | 计量单位缺 `unit_code` 返回 **422**（框架反序列化），而非其他五类的 400 + 10001 | P4 | `POST /business/erp/unit` 只带 `unit_name` | 前端错误提示走错误码分支时拿不到人话；六类同族体验不一致 | `CreateUnitReq.unit_code` 改 `Option<String>` + 自定义「编码必填（单位不自动取号）」400 |
| 5 | 物料 DTO 不返回 `unit_name` / `warehouse_name` | P4 | `GET /business/erp/material/{id}` 响应 | 与 `AGENTS.md` 约定 13「`*_id` 必须同时返回 `*_name`」有偏差；物料列表要多拉一次单位字典才能显示单位名（前端已兜底，非用户可见缺陷） | 物料分页/详情 join `biz_unit` / `biz_warehouse` 补两列 |
| 6 | 手工编码大小写口径不一：SUP/CUS/MAT/WH **原样保留**，AC/ET **归一为大写** | P4 | 六类各建一条小写手工码（如 `e2e08-sup-lc`） | 同一「主数据编码」规则表里出现两种行为；对接方按一类经验写另一类会踩坑 | 统一为「原样保留」（对接企业自有编码更安全），或统一大写并在文档写明 |
| 7 | 计量单位重复编码文案是裸 DB 口径（`编码已存在（唯一约束 uk_biz_unit_code）…`），不点名编码；其他五类是人话 409「编码 X 已存在」 | P4 | `POST /business/erp/unit` 重复码 | 文案没有编码值，用户不知道撞了哪条 | 建单前预检 + 统一 `code_conflict(code)` |
| 8 | 计量单位无 `next-code`，`GET /unit/next-code` 被 `{id}` 路由吃掉返回 400/66006「id 不是 UUID」 | P4 | `GET /business/erp/unit/next-code` | 非缺陷（单位本就不自动），但对齐六类时容易误判为路由丢失 | 文档/前端注释写明「单位不提供取号」；或注册显式路由返回 400 人话 |
| 9 | 序列号批量生成强制 `unit_cost`，卡面未提 | P4 | `POST /business/erp/serial/batch-generate` 不带 unit_cost → 400 | 演练/对接方第一次必踩（本卡踩到）；属文档缺口 | 在接口文档与卡面注明「按期初/其他入库记账，必须录不含税成本单价」 |

无 P0 / P1 / P2。

## 五、修复记录

**commit `8005a872`**（1 个源文件 + 演练脚本）
- `backend/src/business/erp/handler.rs::warehouse_delete`：删除前统计 `biz_material WHERE default_warehouse_id = ? AND deleted_at IS NULL`，>0 → `409 + RESOURCE_CONFLICT`，文案「还有 N 个物料把该仓库设为默认仓，请先调整这些物料的默认仓，或改为停用该仓库」。
- 验证：145 部署 8005a872 后重跑 B —— `②被物料设为默认仓的仓库不能删 HTTP = 409`、`②删除闸文案点名物料/默认仓 含「默认仓」`、`②物料默认仓仍指向原仓库（未悬空）`、随后删物料→仓库可删（200）。`cargo check --bin cloudpivot` 通过、`rustfmt +1.94.1 --edition 2024 --check` 无 diff。
- 未改的 P3（#2 阶梯区间重叠、#3 客户编码可改）需产品口径，已登记建议。

## 六、复现方式

```bash
# 145 上（实例需已部署本次 commit）
bash /opt/cloudpivot/scripts/verify/e2e08_run_all.sh     # 清场 + A/B/C + DB 证据，末行打印 PASS/FAIL 汇总
bash /opt/cloudpivot/scripts/verify/e2e08_cleanup.sh     # 只清 E2E08 前缀演练数据
# 前端「生成编码」按钮（本机 99）
cd frontend && npx tsx --test tests/unit/code05-gen-code-button.test.ts \
  tests/unit/code06-gen-code-button.test.ts tests/unit/code07-gen-code-button.test.ts \
  tests/unit/code08-manual-code-switch.test.ts
```

## 七、证据边界（如实说明）

- **API**：全部经真实 HTTP（`127.0.0.1:8033`，`/api` 前缀），状态码与业务码逐条断言；原始报文在 `raw/`（`auto1_SUP.json`、`dup_SUP.json`、`rs_p1.json`、`cfg_off.json`、`aud_upload.json` …）。
- **DB**：全部用 `psql` 直查（`db-evidence.txt`），不依赖接口自述。
- **前端**：未做浏览器截图（145 nginx 托管的是旧 dist，不含本次页面 chunk）。「生成编码」按钮的**预览 + 填充 + 编辑态禁用 + 编码留空不拦 + 请求体发 null + API 路径带 `/api`** 由 6 个页面的单测（26 条）钉在源码上，86/86 通过；页面列表能力由各 `/page` 接口的 keyword 检索断言覆盖（六类各命中 1 行）。
- 演练期间 145 发生一次他人重启（12:00:06），首轮出现 `HTTP 000` 假失败；已重跑并只保留重启后的干净结果（本次 364/0 为单次连续运行）。
