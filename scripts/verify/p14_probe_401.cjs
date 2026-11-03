/**
 * 一次性探针：列出前端每次导航里返回 401 的请求 URL（用来判断截图时那条
 * `Failed to load resource: 401` 到底是谁）。不产生证据文件，只打印。
 */
const path = require('path');
const fs = require('fs');
const PW = path.resolve(__dirname, '../../frontend/node_modules/@playwright/test');
const { chromium } = require(PW);

const BASE = process.env.P14_BASE || 'http://127.0.0.1:5174';
const PASS = process.env.P14_PASS || 'P14@Passw0rd!';

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage();
  const bad = [];
  page.on('response', (response) => {
    if (response.status() >= 400) {
      bad.push(`${response.status()} ${response.request().method()} ${response.url()}`);
    }
  });
  await page.goto(`${BASE}/login`);
  await page.waitForSelector("input[placeholder='请输入用户名']");
  await page.fill("input[placeholder='请输入用户名']", 'admin');
  await page.fill("input[placeholder='请输入密码']", PASS);
  await page.click('button.t-button--theme-primary');
  await page.waitForURL(/\/(dashboard|change-password)/, { timeout: 30_000 });
  await page.waitForTimeout(3000);
  console.log('登录后失败请求：');
  console.log(bad.length ? bad.join('\n') : '（无）');
  await browser.close();
})();
