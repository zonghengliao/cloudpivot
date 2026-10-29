# L1 修复验收脚本（profile 级凭据）

复现「建 AI 用户必须把 provider key 写进 profile 的 `.env`」这一卡的全部真机验收。
证据文档：`docs/批次D-L1修复-凭据-验证证据.md`。

## 跑法（144，按顺序）

```bash
# 0) 起隔离实例（独立库 cloudpivot_l1fix / 端口 18206 / redis DB5）
bash scripts/verify/l1fix/launch.sh

# 1) 阶段 A：建号 + 检查 .env（需要 ALUN_ADMIN_PASSWORD）
set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
python3 scripts/verify/l1fix/e2e_a.py       # 建 fix-check-ai，打印 .env 键名与权限

# 2) 阶段 B1：造「模板无 key」现场后重建（核心回归）
bash   scripts/verify/l1fix/mk_template.sh
python3 scripts/verify/l1fix/e2e_b1.py

# 3) 阶段 B2/B3：派活 → 真跑 → 三层回传
python3 scripts/verify/l1fix/e2e_b2.py      # 建任务 + dispatch-ai + 轮询（最长 8 分钟）
python3 scripts/verify/l1fix/e2e_b3.py      # 复用已跑完的任务，取三层回传证据

# 4) 阶段 C/D：失败回滚 + 修复接口
python3 scripts/verify/l1fix/e2e_cd.py      # C：临时移走全局 .env → 必须 64014 且零残留
                                            # D：破坏 .env → status 判红 → POST /repair 修回

# 5) 清理
bash scripts/verify/l1fix/cleanup.sh        # 删 AI 用户 / kanban card / 模板 / 附件 / 备份
```

`dbstatus.sh` 打任务/评论/附件/看板的当前状态（排查用）。

## 注意

- 脚本把中间状态写在 `/tmp/l1fix/`（`state.json` / `creds.json` / `server.log`），
  `cleanup.sh` 之后可以整目录删掉。
- `e2e_cd.py` 会**临时移走** `/root/.hermes/.env`（约 3 秒，`finally` 里还原并复核 600）。
  别的 worker 若正在启动会缺全局 key（profile 级 `.env` 不受影响），所以只在隔离
  实例上跑它。
- 生产验收（release 重编 / systemctl restart / 路由 401 / 二进制文案）见证据文档 §八。
