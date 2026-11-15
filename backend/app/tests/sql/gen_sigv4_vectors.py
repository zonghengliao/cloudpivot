#!/usr/bin/env python3
"""生成 AWS SigV4 预签名参考向量（官方 SDK botocore 1.40 实现），供 Rust 单测逐字节对拍。

用法：python3 gen_sigv4_vectors.py

要点（都是实测踩过的坑）：
1. botocore 的 `S3SigV4QueryAuth.add_auth` 每次都用 `get_current_datetime()` 覆盖
   `request.context['timestamp']`，想固定时间只能 monkeypatch 该函数；
2. 签名完成后再调 `canonical_request()`，得到的规范查询串里会混入 `X-Amz-Signature`
   （不是真正被签名的那份），必须钩 `string_to_sign` 抓签名时刻的规范请求；
3. 凭证用**非 AWS 形态**假值：形如 `AKIA...EXAMPLE` 会被终端/日志脱敏层打码，影响比对；
4. 路径键只用 ASCII：botocore 对 S3 的 canonical URI **不做百分号编码**（已知 quirk），
   中文/空格键的编码规则由 Rust 侧单测按 RFC3986 单独覆盖，不与 SDK 对拍。
"""
import json
from datetime import datetime, timezone
from urllib.parse import urlparse, parse_qs

import botocore.auth as ba
from botocore.auth import S3SigV4QueryAuth
from botocore.awsrequest import AWSRequest
from botocore.credentials import Credentials

AK = "cloudpivot-test-ak"
SK = "cloudpivot-test-secret-key"
FIXED = datetime(2026, 10, 28, 3, 14, 15, tzinfo=timezone.utc)
ba.get_current_datetime = lambda: FIXED

CASES = [
    ("path_style_get", "GET", "http://minio.local:9000/mybucket/a/b.txt", "us-east-1", 900),
    ("virtual_host_get", "GET", "https://mybucket.s3.us-west-2.amazonaws.com/a/b.txt", "us-west-2", 900),
    ("path_style_put", "PUT", "http://minio.local:9000/mybucket/public/2026/10/28/uuid7.pdf", "cn-hangzhou", 3600),
    (
        "path_style_get_query",
        "GET",
        "http://minio.local:9000/mybucket/a.txt?response-content-disposition=attachment%3B%20filename%3D%22a.txt%22",
        "us-east-1",
        600,
    ),
]

captured = {}
_orig_sts = ba.SigV4Auth.string_to_sign


def _hook(self, request, canonical_request):
    captured["cr"] = canonical_request
    return _orig_sts(self, request, canonical_request)


ba.SigV4Auth.string_to_sign = _hook

out = []
for name, method, url, region, ttl in CASES:
    req = AWSRequest(method=method, url=url)
    auth = S3SigV4QueryAuth(Credentials(AK, SK), "s3", region, expires=ttl)
    captured.clear()
    auth.add_auth(req)
    signed_url = req.url
    if isinstance(signed_url, bytes):
        signed_url = signed_url.decode()
    q = parse_qs(urlparse(signed_url).query)
    out.append(
        {
            "name": name,
            "method": method,
            "url_in": url,
            "region": region,
            "ttl": ttl,
            "amz_date": q["X-Amz-Date"][0],
            "canonical_request": captured["cr"],
            "string_to_sign": _orig_sts(auth, req, captured["cr"]),
            "signature": q["X-Amz-Signature"][0],
            "signed_url": signed_url,
        }
    )

print(json.dumps(out, ensure_ascii=False, indent=2))
