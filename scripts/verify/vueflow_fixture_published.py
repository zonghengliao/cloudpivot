#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""VUE-FLOW-1 夹具 2：造一条**能发布**的最小流程（开始 → 审批 → 结束）并发布。

用途：验证「只读画布」（`readonly=true`：不画句柄、不能拖、不能拉线）——
已发布版本在编辑器里就是只读打开，这和老画布的只读分支是同一份契约。

用法：
    python3 scripts/verify/vueflow_fixture_published.py --base-url http://127.0.0.1:18098

输出：
    VUE_FLOW_PUBLISHED_DEF_ID=<流程定义 id>
"""
import argparse
import json
import urllib.error
import urllib.request
import uuid


def call(base, path, token, body=None, method=None):
    """发一次请求，返回 data；HTTP 非 2xx 抛错并带响应体"""
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(base + path, data=data, method=method or ('POST' if data else 'GET'))
    req.add_header('Content-Type', 'application/json')
    if token:
        req.add_header('Authorization', 'Bearer ' + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            payload = json.loads(resp.read().decode())
    except urllib.error.HTTPError as exc:  # noqa: PERF203
        raise SystemExit('%s %s → HTTP %s: %s' % (method or 'POST', path, exc.code, exc.read().decode()[:500]))
    if payload.get('code') not in (0, None):
        raise SystemExit('%s %s → code=%s msg=%s' % (method or 'POST', path, payload.get('code'), payload.get('msg')))
    return payload.get('data')


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--base-url', default='http://127.0.0.1:18098')
    ap.add_argument('--user', default='admin')
    ap.add_argument('--password', default='Admin@123456')
    args = ap.parse_args()
    base = args.base_url.rstrip('/')

    token = (call(base, '/api/system/auth/login', None, {'username': args.user, 'password': args.password}) or {}).get('access_token')
    if not token:
        raise SystemExit('登录失败')

    code = 'vf1ro_' + uuid.uuid4().hex[:8]
    created = call(base, '/api/business/flow/def', token, {'flow_code': code, 'flow_name': 'VUE-FLOW-1 只读验证', 'biz_type': 'OTHER'})
    did = created.get('id')
    vid = created.get('version_id')

    nodes = [
        {'key': 's', 'name': '开始', 'type': 'START', 'x': 60, 'y': 160, 'sort_order': 0, 'assignee_type': None},
        {
            'key': 'a1',
            'name': '部门主管审批',
            'type': 'APPROVAL',
            'x': 260,
            'y': 150,
            'sort_order': 0,
            'assignee_type': 'INITIATOR_SUPERIOR',
            'superior_levels': 1,
        },
        {'key': 'e', 'name': '结束', 'type': 'END', 'x': 480, 'y': 160, 'sort_order': 0, 'assignee_type': None},
    ]
    edges = [
        {'key': 'e1', 'from': 's', 'to': 'a1', 'label': None, 'priority': 1, 'is_default': False},
        {'key': 'e2', 'from': 'a1', 'to': 'e', 'label': None, 'priority': 2, 'is_default': False},
    ]
    call(base, '/api/business/flow/version/%s/graph' % vid, token, {'nodes': nodes, 'edges': edges, 'change_note': '只读验证'}, method='PUT')
    call(base, '/api/business/flow/version/%s/validate' % vid, token, {})
    call(base, '/api/business/flow/def/%s/publish' % did, token, {})

    print('VUE_FLOW_PUBLISHED_DEF_CODE=%s' % code)
    print('VUE_FLOW_PUBLISHED_DEF_ID=%s' % did)


if __name__ == '__main__':
    main()
