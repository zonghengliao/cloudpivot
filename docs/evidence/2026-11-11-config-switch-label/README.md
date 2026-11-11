# t_f111fef0 参数配置页布尔开关文案反了（label[0] 是「开启态」）

## 结论
一行修正已落地：`frontend/src/views/support/config/index.vue:106`

```diff
-                    :label="['关闭', '开启']"
+                    :label="['开启', '关闭']"
```

TDesign `Switch` 的 `label` 数组语义是 **`label[0]` = 开启（checked）态文案、`label[1]` = 关闭态文案**
（`node_modules/tdesign-vue-next/es/switch/switch.mjs:115`：
`var label = innerValue.value === activeValue.value ? props2.label[0] : props2.label[1];`）。
旧写法把两段对调，导致**所有 BOOLEAN 参数开关打开时显示「关闭」**。

范围：只改这一处文案。`editorKind` / 保存逻辑 / 其它控件 / 后端全部未动（`git diff` 只有 1 行）。

## 验收结果（都是真跑）

| 验收项 | 结果 | 证据 |
| --- | --- | --- |
| 开关打开显示「开启」、关闭显示「关闭」 | PASS | `04_dom_probe.out` A[1]~A[4]；`01_off_show_close.png`、`02_on_show_open.png` |
| 非成本开关同样正确（announcement.fanout_enabled，库值 true） | PASS | `04_dom_probe.out` A[5]；`03_second_boolean_param.png` |
| `erp.cost.enabled` 库值 true → 「开启」/ false → 「关闭」 | PASS | `12_checks.out`（走真实 PUT 改值：库=true 接口=true / 库=false 接口=false） |
| 旧代码可复现 bug（反证） | BUG REPRODUCED | `04_dom_probe.out` B[6]：stash 掉改动后 checked=false 却显示「开启」 |
| `pnpm typecheck` | exit 0 | `10_typecheck.out` |
| `pnpm build` | exit 0 | `11_build.out` |

## 复跑方法
```bash
cd docs/evidence/2026-11-11-config-switch-label
bash 00_up.sh up      # 建隔离库 costfix_fef0 + 起后端 :18052（自动迁移含 V0385）
bash 01_check.sh      # TDesign 语义 / 源码文案 / 库值 ↔ 接口值 三方核对，期望 ALL PASS
# 前端验收（真浏览器）：frontend/ 下 BACKEND_ORIGIN=http://127.0.0.1:18052 npx vite --port 5199
#   打开 http://127.0.0.1:5199/support/config，搜索 erp.cost.enabled 看开关文案
bash 00_up.sh down    # 停实例 + dropdb
```

## 环境备注
- 隔离库 costfix_fef0 自建，动/删都不影响生产；后端 :18052，前端 dev :5199（`BACKEND_ORIGIN` 覆盖代理目标）。
- 首登强制改密会拦掉所有非改密接口（`middleware/session_guard.rs:105`），所以隔离库里按正常流程把 admin 改成强口令后才拿到可用会话 —— 只动临时库。
- 关态开关文案在 TDesign 默认样式下是浅灰（`t-switch__label` 未选中色），DOM 文本确为「关闭」，可读性受组件默认配色限制，未在本次范围内改样式。

## 文件清单
- `00_up.sh` 起停隔离实例（up/down）
- `01_check.sh` 三方核对脚本（TDesign 语义 + 源码 + 库值/接口值）
- `04_dom_probe.out` 真浏览器 DOM 断言记录（含反证）
- `10_typecheck.out` / `11_build.out` / `12_checks.out` 门禁输出
- `01_off_show_close.png` / `02_on_show_open.png` / `03_second_boolean_param.png` 截图
