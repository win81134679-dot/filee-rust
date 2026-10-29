/**
 * 最小的 Chrome DevTools Protocol 用戶端，走 --remote-debugging-pipe。零相依。
 *
 * 用 pipe 而不是 WebSocket 埠，是因為 Extensions.loadUnpacked 只開放給 pipe 連線
 * （而且要加 --enable-unsafe-extension-debugging）。品牌版 Chrome 137 之後拿掉了
 * --load-extension，這是唯一能在隔離 profile 裡自動掛上未封裝擴充功能的方法。
 *
 * 只給 tools/e2e.mjs 用。擴充功能本身不會載入這個檔案。
 */

import { spawn } from 'node:child_process';

export function launchChrome(chromePath, args) {
  const child = spawn(chromePath, ['--remote-debugging-pipe', '--enable-unsafe-extension-debugging', ...args], {
    // fd 3 = 我們寫給 Chrome，fd 4 = Chrome 寫給我們；訊息以 \0 分隔
    stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'],
  });
  const toChrome = child.stdio[3];
  const fromChrome = child.stdio[4];

  const pending = new Map();
  const listeners = [];
  let nextId = 1;
  let buffer = '';

  fromChrome.on('data', (chunk) => {
    buffer += chunk.toString('utf8');
    let end;
    while ((end = buffer.indexOf('\0')) >= 0) {
      const msg = JSON.parse(buffer.slice(0, end));
      buffer = buffer.slice(end + 1);
      if (msg.id && pending.has(msg.id)) {
        const { resolve, reject, method } = pending.get(msg.id);
        pending.delete(msg.id);
        if (msg.error) reject(new Error(`${method}: ${msg.error.message}`));
        else resolve(msg.result);
      } else {
        for (const fn of listeners) fn(msg);
      }
    }
  });

  function send(method, params = {}, sessionId) {
    const id = nextId++;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject, method });
      toChrome.write(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }) + '\0');
    });
  }

  /** 開一個分頁並 attach（flatten 模式），回傳綁定該分頁的 send。 */
  async function openPage(url = 'about:blank') {
    const { targetId } = await send('Target.createTarget', { url });
    const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
    return {
      sessionId,
      send: (method, params) => send(method, params, sessionId),
      onEvent: (fn) => listeners.push((msg) => msg.sessionId === sessionId && fn(msg)),
    };
  }

  return {
    send,
    openPage,
    close() {
      try {
        child.kill();
      } catch {
        // 已經關掉了
      }
    },
  };
}

/** 在某個分頁的主世界跑 JS，回傳 await 過的結果值。 */
export async function evaluate(page, expression) {
  const result = await page.send('Runtime.evaluate', {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  if (result.exceptionDetails) {
    const e = result.exceptionDetails;
    throw new Error(e.exception?.description ?? e.text);
  }
  return result.result.value;
}
