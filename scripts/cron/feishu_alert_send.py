#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""通用运维告警 → 飞书卡片（直连飞书 Open API，无需 gateway）。

用途：给「144 生产快照一致性巡检」的 SNAPSHOT_AUDIT_NOTIFY_CMD 做落地通道。
凭据与收件频道沿用团队既有告警链路（/root/.hermes/profiles/laoliu/.env 的
FEISHU_APP_ID / FEISHU_APP_SECRET / FEISHU_HOME_CHANNEL）—— 与本机既有的
system_health_card_send.py / cron_health_card_send.py 同一套 v1 卡片口径。

用法：
  python3 feishu_alert_send.py --subject "标题" --stdin            # 正文从 stdin 读
  python3 feishu_alert_send.py --subject "标题" --text "正文"
  python3 feishu_alert_send.py --subject "标题" --stdin --dry-run  # 只打印卡片，不发
  环境变量 DRY_RUN=1 等价于 --dry-run

退出码：0 成功（含 dry-run）；1 发送/回读失败；2 用法或配置错误。
发完 GET 回读校验正文关键词（同团队既有发送器），避免「接口 200 但内容为空」。
"""
import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone

ENV_DEFAULT = '/root/.hermes/profiles/laoliu/.env'
OPEN = 'https://open.feishu.cn/open-apis'
CST = timezone(timedelta(hours=8))
HEAD = '【云枢·快照巡检】'


def load_env(path):
    env = {}
    with open(path, encoding='utf-8') as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith('#') or '=' not in line:
                continue
            k, v = line.split('=', 1)
            env[k.strip()] = v.strip().strip('"').strip("'")
    return env


def http_json(url, payload=None, headers=None, method='POST'):
    data = None
    if payload is not None:
        data = json.dumps(payload).encode('utf-8')
    req = urllib.request.Request(url, data=data, headers=headers or {}, method=method)
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read().decode('utf-8'))


def tenant_token(env):
    r = http_json(f'{OPEN}/auth/v3/tenant_access_token/internal',
                  {'app_id': env['FEISHU_APP_ID'], 'app_secret': env['FEISHU_APP_SECRET']},
                  {'Content-Type': 'application/json'})
    if r.get('code') != 0:
        raise RuntimeError(f"取 tenant_access_token 失败: {r.get('code')} {r.get('msg')}")
    return r['tenant_access_token']


def build_card(subject, body, template='red'):
    stamp = datetime.now(CST).strftime('%Y-%m-%d %H:%M:%S CST')
    return {
        'config': {'wide_screen_mode': True},
        'header': {
            'template': template,
            'title': {'tag': 'plain_text', 'content': f'{HEAD}{subject}'},
        },
        'elements': [
            {'tag': 'div', 'text': {'tag': 'lark_md', 'content': body}},
            {'tag': 'hr'},
            {'tag': 'note', 'elements': [
                {'tag': 'plain_text', 'content': f'发送时间 {stamp} · 巡检只报不修（设计文档 AP-8）'},
            ]},
        ],
    }


def _plain(text):
    """去掉 markdown 标记，用于回读比对（飞书落库时 `**` 会被吃掉）。"""
    return re.sub(r'[`*#>\[\]\\]', '', text)


def message_text(items):
    """把消息列表拼成可搜索文本；卡片 body.content 是「JSON 字符串」，解一层才有中文。"""
    parts = []
    for it in items:
        c = (it.get('body') or {}).get('content') or ''
        try:
            parts.append(json.dumps(json.loads(c), ensure_ascii=False))
        except Exception:  # noqa: BLE001
            parts.append(c)
    return '\n'.join(parts)


def main():
    ap = argparse.ArgumentParser(description='发送运维告警飞书卡片')
    ap.add_argument('--subject', required=True)
    ap.add_argument('--text', default=None)
    ap.add_argument('--stdin', action='store_true', help='正文从标准输入读')
    ap.add_argument('--template', default='red')
    ap.add_argument('--env-file', default=ENV_DEFAULT)
    ap.add_argument('--dry-run', action='store_true', help='只打印卡片 JSON，不发送')
    ap.add_argument('--self-test-message-id', default=None,
                    help='只读：对一条已发送消息重跑回读校验（配合 --subject 当探针，不发消息）')
    args = ap.parse_args()

    if args.stdin:
        body = sys.stdin.read()
    else:
        body = args.text or ''
    body = body.strip()
    if not body:
        print('正文为空，拒绝发送', file=sys.stderr)
        return 2

    if args.self_test_message_id:
        env = load_env(args.env_file)
        token = tenant_token(env)
        rb = http_json(f'{OPEN}/im/v1/messages/{args.self_test_message_id}', None,
                       {'Authorization': f'Bearer {token}'}, method='GET')
        items = rb.get('data', {}).get('items') or []
        probe = _plain(body.splitlines()[0])[:12]
        got = _plain(message_text(items))
        print(f'code={rb.get("code")} items={len(items)} probe={probe!r} 命中={probe in got}')
        return 0 if (probe and probe in got) else 1

    card = build_card(args.subject, body, args.template)
    dry = args.dry_run or os.environ.get('DRY_RUN') == '1'
    if dry:
        print(json.dumps(card, ensure_ascii=False, indent=2))
        print('DRY_RUN：未发送', file=sys.stderr)
        return 0

    try:
        env = load_env(args.env_file)
        for k in ('FEISHU_APP_ID', 'FEISHU_APP_SECRET', 'FEISHU_HOME_CHANNEL'):
            if not env.get(k):
                print(f'缺少配置 {k}（{args.env_file}）', file=sys.stderr)
                return 2
        chat_id = env['FEISHU_HOME_CHANNEL']
        token = tenant_token(env)
        r = http_json(
            f'{OPEN}/im/v1/messages?receive_id_type=chat_id',
            {'receive_id': chat_id, 'msg_type': 'interactive',
             'content': json.dumps(card, ensure_ascii=False)},
            {'Authorization': f'Bearer {token}', 'Content-Type': 'application/json'})
        if r.get('code') != 0:
            print(f"发送失败: {r.get('code')} {r.get('msg')}", file=sys.stderr)
            return 1
        mid = r['data']['message_id']
        # 回读校验：正文首行关键词必须出现在落库内容里（去掉 markdown 标记再比）
        probe = _plain(body.splitlines()[0])[:12]
        rb = http_json(f'{OPEN}/im/v1/messages/{mid}', None,
                       {'Authorization': f'Bearer {token}'}, method='GET')
        items = rb.get('data', {}).get('items') or []
        got = _plain(message_text(items))
        if rb.get('code') != 0:
            print(f'回读失败: {rb.get("code")} {rb.get("msg")}', file=sys.stderr)
            return 1
        if probe and probe not in got:
            print(f'回读校验未命中关键词 {probe!r}（message_id={mid}）', file=sys.stderr)
            return 1
        print(f'✅ 已发送 feishu:{chat_id[:10]}… message_id={mid}')
        return 0
    except urllib.error.URLError as exc:
        print(f'网络错误: {exc}', file=sys.stderr)
        return 1
    except Exception as exc:  # noqa: BLE001
        print(f'发送异常: {exc}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
