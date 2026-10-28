# 批次 A · A1 存储底座的验证脚本

三个脚本都是**真跑过**的（2026-10-28，172.18.10.144 / PostgreSQL 18.6），可直接复用到
CI 或 A2 的回归里。

| 文件 | 用途 | 前置条件 |
|------|------|----------|
| `verify_v0040.sh` | V0040 的 15 条约束断言（唯一默认、远端/本地互斥、非法枚举、策略边界、`sys_file` 扩展列） | 目标库已执行过 V0040；`ALUN_DATABASE_USER` / `PGPASSWORD` 已注入 |
| `curl_crud_test.sh` | 19 条接口断言（配置 CRUD、设默认、删除保护、测试连接、策略修改、掩码与 SSRF） | test profile 实例监听 127.0.0.1:8025，`ALUN_ADMIN_PASSWORD` 已注入 |
| `gen_sigv4_vectors.py` | 用**官方 SDK**（botocore）生成 SigV4 预签名参考向量，供 `sigv4.rs` 单测逐字节对拍 | `pip install botocore`（1.40+） |

## 用法

```bash
# 1) 结构断言（在 cloudpivot_test 库上，绝不碰 cailu / cailu_dev / prod）
set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
export PGPASSWORD="$ALUN_DATABASE_PASSWORD"
bash backend/tests/sql/verify_v0040.sh cloudpivot_test

# 2) 接口断言（先按 test profile 起实例）
(cd backend && ALUN_PROFILE=test ./target/debug/cloudpivot &)   # 或 scripts/dev-up.sh 的 test 变体
bash backend/tests/sql/curl_crud_test.sh

# 3) 重新生成 SigV4 参考向量（改了签名实现时才需要，生成值粘回 sigv4.rs 单测）
python3 backend/tests/sql/gen_sigv4_vectors.py
```

## 为什么 `verify_v0040.sh` 要按「期望失败」写断言

V0040 的价值一半在表、一半在**约束**：`uk_sys_storage_config_default` 能否拦住第二个默认后端、
`ck_sys_storage_config_remote` 能否拦住「S3 忘配密钥 / 本地误填密钥」，这类问题只有
「故意写非法数据并断言报错」才能证明。脚本对每条非法插入检查错误串里出现的**约束名**，
避免「因为别的原因失败也被算作通过」。
