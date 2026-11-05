#!/usr/bin/env node
/** 诊断：SSE fetch 流在 Playwright 里到底会触发哪些事件（1 页，12 秒窗口） */
'use strict';
import fs from 'node:fs';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const pw = require('/opt/cloudpivot/frontend/node_modules/@playwright/test');
const BASE = process.env.WALK_BASE || 'http://127.0.0.1:5140';
const PASSWORD = fs.readFileSync('/tmp/p1fix6-iso/admin.pw', 'utf8').trim();

(async () => {
  const lg = await (
    await fetch(`${BASE}/api/system/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: 'admin', password: PASSWORD }),
    })
  ).json();
  const token = lg.data.access_token;
  const browser = await pw.chromium.launch();
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  await ctx.addInitScript(([a, r]) => {
    localStorage.setItem('x_access_token', a);
    localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('cp-onboarding-done', '1');
    localStorage.setItem('x_guide_done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  }, [token, lg.data.refresh_token]);
  const page = await ctx.newPage();
  const isStream = (u) => u.includes('/api/business/notice/stream');
  const t0 = Date.now();
  const log = (k, extra) => console.log(`${((Date.now() - t0) / 1000).toFixed(1)}s ${k} ${extra ?? ''}`);

  page.on('request', (r) => isStream(r.url()) && log('request', r.method() + ' ' + r.url().slice(0, 80)));
  page.on('response', (r) => isStream(r.url()) && log('response', r.status()));
  page.on('requestfinished', (r) => isStream(r.url()) && log('requestfinished'));
  page.on('requestfailed', (r) => isStream(r.url()) && log('requestfailed', r.failure()?.errorText));

  const cdp = await ctx.newCDPSession(page);
  await cdp.send('Network.enable');
  cdp.on('Network.responseReceived', (e) => {
    if (isStream(e.response.url)) log('CDP.responseReceived', `${e.response.status} ${e.response.mimeType}`);
  });
  cdp.on('Network.requestWillBeSent', (e) => {
    if (isStream(e.request.url)) log('CDP.requestWillBeSent', e.request.method);
  });

  await page.goto(`${BASE}/dashboard`, { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(12000);
  const info = await page.evaluate(() => ({
    // 顶栏角标：有实时连接时 connected=true（store 里没有挂到 window，这里只看得到 DOM）
    bodyHasAside: !!document.querySelector('.basic-layout__aside'),
  }));
  console.log('page:', JSON.stringify(info));
  await browser.close();
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
