/**
 * 把 docs/架構圖/HTML/*.html 渲染成 A2 橫式滿版的 PNG（300 DPI）與單頁 A2 PDF（由 Tasker 專案那支改的）。
 *
 * 版面是 A2 橫式 594×420 mm = 2245×1587 CSS px（96 DPI）。PNG 用 deviceScaleFactor = 300/96
 * 截圖，實際約 7016×4960 像素；pHYs 的 300 DPI 標記由 工具/set_png_dpi.py 寫入（腳本最後自己叫）。
 * PDF 是 A2 的向量單頁，背景滿版、無邊界。HTML 的 body 要是 2245×1587、SVG viewBox 1280×904。
 *
 * 用法：node 工具/render_diagram_a2.mjs docs/架構圖/HTML/架構圖.html [輸出資料夾]
 * 輸出：docs/架構圖/PNG/<名稱>.png、docs/架構圖/PDF/<名稱>.pdf；給了輸出資料夾就兩個都放那裡
 */

import { existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { tmpdir } from 'node:os';

import { evaluate, launchChrome } from './cdp.mjs';

const WIDTH = 2245; // 594 mm at 96 DPI
const HEIGHT = 1587; // 420 mm
const PAPER_W_IN = 594 / 25.4;
const PAPER_H_IN = 420 / 25.4;
const DPI = 300;

const CHROME = [
  join(process.env.ProgramFiles ?? '', 'Google/Chrome/Application/chrome.exe'),
  join(process.env['ProgramFiles(x86)'] ?? '', 'Google/Chrome/Application/chrome.exe'),
  join(process.env.LOCALAPPDATA ?? '', 'Google/Chrome/Application/chrome.exe'),
].find((p) => existsSync(p));

async function main() {
  const input = resolve(process.argv[2] ?? '');
  if (!existsSync(input)) throw new Error(`找不到 ${input}`);
  const name = basename(input, '.html');
  const base = dirname(dirname(input)); // HTML\ 的上一層
  // 第二個參數給資料夾時，PNG 與 PDF 直接放那裡（例如 匯出\）；沒給就放 HTML\ 旁的 PNG\、PDF\
  const outDir = process.argv[3] ? resolve(process.argv[3]) : null;
  const pngDir = outDir ?? join(base, 'PNG');
  const pdfDir = outDir ?? join(base, 'PDF');
  mkdirSync(pngDir, { recursive: true });
  mkdirSync(pdfDir, { recursive: true });

  const profile = join(tmpdir(), `filee-render-${process.pid}`);
  const chrome = launchChrome(CHROME, ['--headless=new', `--user-data-dir=${profile}`, '--no-first-run', '--hide-scrollbars', 'about:blank']);
  try {
    const page = await chrome.openPage();
    await page.send('Page.enable');
    await page.send('Emulation.setDeviceMetricsOverride', {
      width: WIDTH, height: HEIGHT, deviceScaleFactor: DPI / 96, mobile: false,
    });
    await page.send('Page.navigate', { url: pathToFileURL(input).href });
    // 等 Google Fonts（Geist、Noto Sans TC 分片）真的載入，不然截到的是後備字型
    const deadline = Date.now() + 20000;
    while (Date.now() < deadline) {
      const ready = await evaluate(page, `document.readyState === 'complete' && document.fonts.status === 'loaded'`).catch(() => false);
      if (ready) break;
      await new Promise((r) => setTimeout(r, 300));
    }
    await evaluate(page, 'document.fonts.ready.then(() => true)');
    // 只用在小標籤的字型不一定會被瀏覽器主動載入，明確叫它載
    await evaluate(page, `Promise.all(['600 16px "Geist"','500 8px "Geist Mono"','400 12px "Geist Mono"','400 40px "Instrument Serif"','500 12px "Noto Sans TC"'].map(f => document.fonts.load(f))).then(() => true)`);
    const fonts = await evaluate(page, `['Geist','Geist Mono','Noto Sans TC','Instrument Serif'].map(f => f + ':' + document.fonts.check('16px "' + f + '"')).join(' ')`);

    const shot = await page.send('Page.captureScreenshot', {
      format: 'png',
      clip: { x: 0, y: 0, width: WIDTH, height: HEIGHT, scale: 1 },
      captureBeyondViewport: false,
    });
    const png = join(pngDir, `${name}.png`);
    writeFileSync(png, Buffer.from(shot.data, 'base64'));

    const pdf = await page.send('Page.printToPDF', {
      paperWidth: PAPER_W_IN,
      paperHeight: PAPER_H_IN,
      marginTop: 0, marginBottom: 0, marginLeft: 0, marginRight: 0,
      printBackground: true,
      pageRanges: '1',
    });
    const pdfPath = join(pdfDir, `${name}.pdf`);
    writeFileSync(pdfPath, Buffer.from(pdf.data, 'base64'));

    // pHYs = 300 DPI（像素不動）：office venv 有 PIL
    const { spawnSync } = await import('node:child_process');
    const py = join(process.env.USERPROFILE ?? '', '.venvs', 'office', 'Scripts', 'python.exe');
    const dpiScript = join(dirname(new URL(import.meta.url).pathname.replace(/^\/(\w:)/, '$1')), 'set_png_dpi.py');
    const res = spawnSync(py, ['-I', '-X', 'utf8', decodeURIComponent(dpiScript), png, '--dpi', String(DPI)], { encoding: 'utf8' });
    if (res.status !== 0) console.error(`set_png_dpi 失敗：${res.stderr || res.stdout}`);
    console.log(`字型：${fonts}`);
    console.log(`PNG：${png}`);
    console.log(`PDF：${pdfPath}`);
  } finally {
    chrome.close();
    setTimeout(() => rmSync(profile, { recursive: true, force: true }), 1500);
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
