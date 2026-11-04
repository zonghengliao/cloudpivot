#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""VUE-FLOW-1 夹具：造一张覆盖 9 种节点类型的草稿流程图。

用途：真机对比「老画布 vs Vue Flow 画布」的外观与交互。图里有 9 种节点各一个、
10 条边（含兜底分支 / 回边），足以覆盖：类型色、紧凑排版（开始/结束 96×44）、
菱形网关（条件/并行 132×84）、标签（条件分支）、默认分支虚线。

用法：
    python3 scripts/verify/vueflow_fixture.py --base-url http://127.0.0.1:18098 \
        --user admin --password 'Admin@123456'

输出（可直接给其它脚本用）：
    VUE_FLOW_DEF_ID=<流程定义 id>
    VUE_FLOW_VERSION_ID=<草稿版本 id>
"""
import argparse
import json
import urllib.request
import uuid

# 节点：key / 名称 / 类型 / 坐标（画布逻辑坐标，10px 网格）
NODES = [
    ('s', '开始', 'START', 40, 210),
    ('task1', '填写申请', 'TASK_NODE', 200, 200),
    ('cond1', '金额分级', 'CONDITION', 420, 190),
    ('appr1', '部门主管审批', 'APPROVAL', 640, 80),
    ('appr2', '财务复核', 'APPROVAL', 640, 200),
    ('cs1', '总经理会签', 'COUNTERSIGN', 640, 330),
    ('par1', '并行处理', 'PARALLEL', 900, 140),
    ('cc1', '抄送行政', 'CC', 1120, 30),
    ('note1', '通知申请人', 'NOTIFY_NODE', 1120, 170),
    ('e', '结束', 'END', 1340, 190),
]

# 边：(key, from, to, 标签, 是否兜底)
EDGES = [
    ('e1', 's', 'task1', None, False),
    ('e2', 'task1', 'cond1', None, False),
    ('e3', 'cond1', 'appr1', '金额 > 1000', False),
    ('e4', 'cond1', 'appr2', '金额 ≤ 1000', False),
    ('e5', 'cond1', 'cs1', '其他情况', True),
    ('e6', 'appr1', 'par1', None, False),
    ('e7', 'appr2', 'par1', None, False),
    ('e8', 'cs1', 'cc1', None, False),
    ('e9', 'par1', 'note1', None, False),
    ('e10', 'note1', 'e', None, False),
]

# 需要配审批/负责人的类型（后端 R14 / R20）
ASSIGNEE_TYPES = {'APPROVAL', 'COUNTERSIGN', 'CC', 'TASK_NODE', 'NOTIFY_NODE'}


def call(base, path, token, body=None, method=None):
    """发一次请求，返回解析后的 data（HTTP 非 2xx 时抛错并带响应体）"""
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(base + path, data=data, method=method or ('POST' if data else 'GET'))
    req.add_header('Content-Type', 'application/json')
    if token:
        req.add_header('Authorization', 'Bearer ' + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            payload = json.loads(resp.read().decode())
    except urllib.error.HTTPError as exc:  # noqa: PERF203
        raise SystemExit('%s %s → HTTP %s: %s' % (method or 'POST', path, exc.code, exc.read().decode()[:400]))
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
        raise SystemExit('登录失败：拿不到 access_token')

    code = 'vf1_' + uuid.uuid4().hex[:8]
    created = call(base, '/api/business/flow/def', token, {'flow_code': code, 'flow_name': 'VUE-FLOW-1 画布验证', 'biz_type': 'OTHER'})
    did = created.get('id')
    vid = created.get('version_id')
    if not vid:
        versions = call(base, '/api/business/flow/def/%s/versions' % did, token) or {}
        for ver in versions.get('list') or []:
            if ver.get('version_status') == 'DRAFT':
                vid = ver.get('id')
                break
        vid = vid or ((versions.get('list') or [{}])[0].get('id'))
    if not did or not vid:
        raise SystemExit('建流程定义失败：did=%s vid=%s' % (did, vid))

    nodes = []
    for key, name, ntype, x, y in NODES:
        node = {
            'key': key,
            'name': name,
            'type': ntype,
            'x': x,
            'y': y,
            'sort_order': 0,
            'assignee_type': 'INITIATOR_SUPERIOR' if ntype in ASSIGNEE_TYPES else None,
            'superior_levels': 1,
            'approval_mode': 'ALL' if ntype == 'COUNTERSIGN' else None,
            'approve_ratio': None,
            'reject_veto': True,
            'empty_assignee': 'ERROR' if ntype == 'TASK_NODE' else 'SKIP',
            'self_approval': 'SKIP',
            'allow_robot': False,
            'join_mode': 'AND',
            'fork_mode': 'AND',
        }
        if ntype == 'TASK_NODE':
            # 后端 ck_biz_flow_node_task_config：必须有 title_template + on_complete
            node['config'] = {'task': {'title_template': '填写{{instance.title}}', 'on_complete': 'DIRECT'}}
        if ntype == 'NOTIFY_NODE':
            # 后端 ck_biz_flow_node_notify_config：targets 1–5 项 + when + 模板二选一
            node['config'] = {
                'notify': {
                    'when': 'ON_ENTER',
                    'targets': [{'type': 'INITIATOR'}],
                    'inline': {'title': '流程通知', 'content': '你有一条流程待处理'},
                }
            }
        nodes.append(node)

    edges = []
    for idx, (key, src, dst, label, is_default) in enumerate(EDGES):
        edges.append({
            'key': key,
            'from': src,
            'to': dst,
            'condition': None,
            'label': label,
            'priority': idx + 1,
            'is_default': is_default,
        })

    call(base, '/api/business/flow/version/%s/graph' % vid, token, {'nodes': nodes, 'edges': edges, 'change_note': 'VUE-FLOW-1 夹具'}, method='PUT')

    print('VUE_FLOW_DEF_CODE=%s' % code)
    print('VUE_FLOW_DEF_ID=%s' % did)
    print('VUE_FLOW_VERSION_ID=%s' % vid)


if __name__ == '__main__':
    main()
