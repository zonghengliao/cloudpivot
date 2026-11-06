# P1-FIX-12B 证据：联动规则状态显示中文 + 动作白名单说明（前端）

卡：`t_3bcfd24c`（承接后端 `t_08ca10e4` / commit `f1f3375` 下发的 `status_labels`）。

## 改动（只 4 个前端文件）

| 文件 | 改了什么 |
|------|----------|
| `frontend/src/api/business/flow/linkage.ts` | 补两个后端已下发的字段：`LinkageTrigger.status_labels[{code,label}]`、`LinkageRule.trigger_status_label` |
| `frontend/src/views/business/flow/linkage/linkage-logic.ts` | `statusOptions` 出「中文（编码）」；新增 `statusLabelOf` / `ruleTriggerStatusText` / `implementedActionCount` / `actionWhitelistNote` |
| `frontend/src/views/business/flow/linkage/index.vue` | 列表状态列显示中文 + 编码小字 + 悬停 tooltip；「目标动作（白名单）」label 挂 ⓘ tooltip + 控件下方常驻说明（X 由 `implemented=true` 现算） |
| `frontend/tests/unit/flow-linkage.test.ts` | +4 条单测（下拉文案 / 查表回落 / 列表取词 / 白名单说明计数），fixture 补 `status_labels`、`trigger_status_label` |

口径要点：
- 字典只有后端一份，前端只查表；查不到一律回落编码（不显示空白、不编造中文）。
- 下拉 `value` 仍是编码（落库/后端校验认它），中文只在 `label` 里。
- 「当前 X 个已实现」取 `actions[].implemented === true` 的数量（不是动作总数：占位动作在下拉里置灰，不该算「能用」）。

## 文件

| 文件 | 内容 |
|------|------|
| `01_list_status_zh_with_tooltip.png` | 列表状态列：`已过账` 中文 tag + `POSTED` 编码小字，悬停显示「状态编码：POSTED」 |
| `02_edit_dialog_status_zh.png` | 编辑弹窗：唯一键只读，触发状态显示 `已过账（POSTED）` |
| `03_action_whitelist_note.png` | 新增弹窗：「目标动作（白名单）」label 带 ⓘ，控件下方说明「动作由系统内置（当前 4 个已实现），不能自行新增；如需新动作（如发短信、调外部接口）需开发。每个动作的参数可自由配置。」 |
| `04_form_status_options_zh.png` | 触发状态下拉 5 项全部「中文（编码）」 |
| `05_api_status_labels.txt` | 真实 token curl：`GET /business/flow/linkage` → 15/15 触发点带 `status_labels`、3 条规则带 `trigger_status_label`（硬断言 exit 0） |
| `06_unit_tests.txt` | `pnpm test:unit` 全量 860 条全绿 |
| `07_typecheck_build.txt` | `vue-tsc --noEmit` + `vite build`（outDir 指仓库外）通过，产物里命中新文案 |
| `08_self_check.txt` | 自检脚本 `scripts/verify/p1fix12b_linkage_zh_check.sh` 实跑（4 条接口断言 + 13 条单测全绿）+ 负样本（删掉 `status_labels` 后 4 条断言全红，证明断言不是空转） |
| `09_preview_status_zh.png` | 预览影响弹窗：`收货入库单 STOCK_IN → 已过账 POSTED`（同一页面里其它露状态的地方也一并用中文，不留「半中半码」） |

## 真机环境

- 独立后端：`127.0.0.1:18098`，二进制 `/opt/cloudpivot/backend/target/debug/cloudpivot`（含 12A，`strings` 命中 `status_labels` 43 处），库 `cloudpivot_p1fix12b`（`createdb -T cp_p1fix12a` 克隆）。
  > 注意：共享树上 `127.0.0.1:18097` 那台 09:30 启动的进程跑的是 **12A 之前的旧二进制**（`GET` 无 `status_labels`），12A 的库/脚本没变，但进程一直没重启。本卡没用它。
- 前端：`vite dev`（源码即产物）`127.0.0.1:5186`，`BACKEND_ORIGIN=http://127.0.0.1:18098`；**没有在 `/opt/cloudpivot/frontend` 里 build 到 `dist`**（那是生产站点目录，验收卡不发版）。
- 浏览器：CDP 真机，视口 1600×1000，`localStorage.x_access_token` 注入真实登录令牌（口令不进日志）。

## 注意（给下一位）

- 克隆库的 `admin` 口令与 `cp_p1fix12a` 建库时不同（12A 脚本首登改过密）→ 抄 `cloudpivot_test` 的 argon2 串并 `must_change_password=false` 再登录。
- 无写入动作：本轮只读列表/字典，未新增或修改任何规则（库里 3 条规则原样）。
