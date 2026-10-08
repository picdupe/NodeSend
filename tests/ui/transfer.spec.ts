// UI-only IPC fixtures; native protocol tests independently use real sockets.
import { test, expect } from '@playwright/test';
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const self = 'a'.repeat(64); const remote = 'b'.repeat(64);
    const entries = [{ path: '项目资料.pdf', size: 2400000, directory: false, modified_ms: 0 }, { path: '封面.png', size: 850000, directory: false, modified_ms: 0 }];
    const state: any = { node_id: self, display_name: '我的工作站', tcp_port: 58082, quic_port: 58082, addresses: ['192.168.1.8'], receive_directory: 'D:/Downloads/NodeSend', receive_policy: 'ask', discovery: { mdns: true, udp: true, warnings: [] }, peers: [{ node_id: remote, display_name: 'Studio Mac', endpoints: [{ address: '192.168.1.12', tcp_port: 58082, quic_port: 58082, source: 'mdns', last_seen: 1, error: null }], online: true, trusted: false, verified: true }], tasks: [] };
    const calls: any[] = [];
    const browserConnection = { id: 'browser-test', name: 'Studio Mac', protocol: 'nodesend', address: remote, username: null, read_only: false, connected: true };
    (window as any).__testState = state; (window as any).__testCalls = calls;
    (window as any).__TAURI_INTERNALS__ = { metadata: { currentWebview: { label: 'main' }, currentWindow: { label: 'main' } }, transformCallback: () => 1, unregisterCallback: () => {}, invoke: async (cmd: string, args: any) => {
      calls.push({ cmd, args });
      if (cmd === 'get_network_status') return structuredClone(state);
      if (cmd === 'get_node_info') return { node_id: self, http_enabled: false, http_listen: '0.0.0.0:58080' };
      if (cmd === 'list_browser_connections') return calls.some((call: any) => call.cmd === 'connect_browser') ? [browserConnection] : [];
      if (cmd === 'list_shares') return [];
      if (cmd === 'connect_browser') return browserConnection;
      if (cmd === 'browse_browser') return { connection_id: browserConnection.id, path: args.path,
        can_write: Boolean(args.path), entries: args.path ? [{ name: 'hello.txt', path: 'share-id/hello.txt', is_dir: false, size: 17, modified: '2026-09-29T00:00:00Z', can_write: false }] : [{ name: 'Shared', path: 'share-id', is_dir: true, size: 0, modified: null, can_write: false }] };
      if (cmd === 'choose_send_files') return ['D:/文件/项目资料.pdf', 'D:/文件/封面.png'];
      if (cmd === 'preview_transfer') return { entries, chunk_size: 262144, verify_hash: false };
      if (cmd === 'choose_receive_directory') return 'D:/Received';
      if (cmd === 'send_files') { state.tasks.unshift({ id: 'out-test', peer_id: remote, peer_name: 'Studio Mac', direction: 'send', status: 'waiting', entries, total_bytes: 3250000, completed_bytes: 0, transport: 'quic', error: null, destination: null, events: ['已验证接收方身份'], created_at: '2026-09-28T12:00:00Z', verify_hash: args.verifyHash }); return 'out-test'; }
      if (cmd === 'decide_incoming_transfer') { state.tasks.find((t: any) => t.id === args.id).status = args.accepted ? 'accepted' : 'rejected'; return; }
      if (cmd === 'transfer_action') { state.tasks.find((t: any) => t.id === args.id).status = args.action === 'pause' ? 'paused' : args.action === 'cancel' ? 'cancelled' : 'transferring'; return; }
      if (cmd === 'add_endpoint') {
        if (args.address === '203.0.113.7:58082') {
          if (!args.allowPublic) throw { code: 'FORBIDDEN', message: '禁止访问: 公网路径需要显式允许' };
          const peer = { node_id: 'c'.repeat(64), display_name: 'Public Node', endpoints: [{ address: '203.0.113.7', tcp_port: 58082, quic_port: 58082, source: 'manual', last_seen: 1, error: null }], online: true, trusted: false, verified: true };
          state.peers.push(peer); return peer;
        }
        throw { code: 'CONNECTION_ERROR', message: '无法连接此地址，请检查端口' };
      }
      return 1;
    }};
  });
  await page.goto('/');
});
test('mobile content buttons route to distinct pickers and animate only icons', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.reload();
  const buttons = page.locator('.send-type-buttons');
  for (const name of ['文件', '媒体', '应用']) {
    await buttons.getByRole('button', { name, exact: true }).click();
  }
  const kinds = await page.evaluate(() => (window as any).__testCalls
    .filter((call: any) => call.cmd === 'choose_send_files').map((call: any) => call.args.kind));
  expect(kinds).toEqual(['files', 'media', 'apps']);
  const media = buttons.getByRole('button', { name: '媒体', exact: true });
  await media.hover();
  await expect(media).toHaveCSS('background-color', 'rgb(248, 249, 252)');
  await expect(media).toHaveCSS('-webkit-tap-highlight-color', 'rgba(0, 0, 0, 0)');
});

test('mobile native clipboard text enters send selection without desktop clipboard calls', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.reload();
  await page.evaluate(() => {
    const native = (window as any).__TAURI_INTERNALS__;
    const original = native.invoke;
    native.invoke = async (cmd: string, args: any) => {
      if (cmd === 'read_clipboard_files') return ['/data/user/0/com.picdupe.nodesend/cache/shared-text.txt'];
      return original(cmd, args);
    };
  });
  await page.getByRole('button', { name: '剪切板', exact: true }).click();
  await expect(page.locator('.selection-row')).toContainText('shared-text.txt');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).__testCalls
    .filter((call: any) => call.cmd.startsWith('plugin:clipboard-manager')))).toEqual([]);
});

for (const reducedMotion of ['reduce', 'no-preference'] as const) {
  test(`refresh rotates and rests diagonally with motion preference ${reducedMotion}`, async ({ page }) => {
    await page.emulateMedia({ reducedMotion });
    const button = page.getByRole('button', { name: '刷新设备与任务' });
    const icon = button.locator('svg');
    await button.click();
    await expect(button).toHaveAttribute('aria-busy', 'true');
    await expect(icon).toHaveCSS('animation-name', 'refresh-spin');
    const initial = await icon.evaluate(element => getComputedStyle(element).transform);
    await expect.poll(() => icon.evaluate(element => getComputedStyle(element).transform)).not.toBe(initial);
    await expect(button).toHaveAttribute('aria-busy', 'false');
    const angle = await icon.evaluate(element => {
      const matrix = new DOMMatrix(getComputedStyle(element).transform);
      return Math.atan2(matrix.b, matrix.a) * 180 / Math.PI;
    });
    expect(angle).toBeCloseTo(60, 1);
  });
}

test('device favorites persist, remain visible offline and can be removed', async ({ page }) => {
  const favoriteButton = page.getByRole('button', { name: '查看收藏设备' });
  await favoriteButton.click();
  await expect(page.getByText('还没有收藏设备')).toBeVisible();
  await page.getByRole('button', { name: '关闭收藏夹' }).click();
  await page.locator('.device-card').filter({ hasText: 'Studio Mac' }).click({ button: 'right' });
  await page.getByRole('button', { name: '加入收藏夹' }).click();
  await expect(page.getByRole('button', { name: '取消收藏' })).toHaveAttribute('aria-pressed', 'true');
  await page.getByRole('button', { name: '关闭设备详情' }).click();
  await page.reload();
  await favoriteButton.click();
  const dialog = page.getByRole('dialog', { name: '收藏设备' });
  await expect(dialog.locator('.device-card')).toHaveCount(1);
  await expect(dialog.locator('.device-favorite-mark')).toHaveCount(0);
  await expect(dialog).toContainText('Studio Mac');
  await page.evaluate(() => { (window as any).__testState.peers = []; });
  await expect(dialog.getByLabel('暂未连接')).toBeVisible();
  await expect(dialog).toContainText('192.168.1.12');
  await dialog.locator('.device-card').click({ button: 'right' });
  const details = page.getByRole('dialog', { name: 'Studio Mac' });
  await expect(details).toContainText('192.168.1.12');
  await expect(details).toContainText('暂未连接');
  await page.getByRole('button', { name: '关闭设备详情' }).click();
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole('button', { name: '取消收藏' })).toBeDisabled();
  await dialog.locator('.device-card').click();
  await dialog.getByRole('button', { name: '取消收藏' }).click();
  await expect(dialog).toContainText('还没有收藏设备');
  await page.reload();
  await favoriteButton.click();
  await expect(page.getByText('还没有收藏设备')).toBeVisible();
});

test('favorites send uses the selected files and requires a selected device', async ({ page }) => {
  await page.locator('.device-card').click({ button: 'right' });
  const details = page.getByRole('dialog');
  await expect(details.getByRole('button', { name: '关闭', exact: true })).toHaveCount(0);
  await details.getByRole('button', { name: '加入收藏夹' }).click();
  await page.getByRole('button', { name: '关闭设备详情' }).click();
  await page.getByRole('button', { name: '查看收藏设备' }).click();
  const dialog = page.getByRole('dialog', { name: '收藏设备' });
  await expect(dialog.getByRole('button', { name: '关闭', exact: true })).toHaveCount(0);
  await expect(dialog.getByRole('button', { name: '发送', exact: true })).toBeDisabled();
  await dialog.locator('.device-card').click();
  await expect(dialog.getByRole('button', { name: '发送', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: '关闭收藏夹' }).click();
  await page.locator('.send-type-buttons').getByRole('button', { name: '文件', exact: true }).click();
  await expect(page.locator('.selection-row')).toHaveCount(2);
  await page.getByRole('button', { name: '查看收藏设备' }).click();
  await expect(dialog.locator('.device-card')).toHaveAttribute('aria-pressed', 'true');
  await dialog.getByRole('button', { name: '发送', exact: true }).click();
  await expect(page.locator('.task-card .badge')).toHaveText('等待接收确认');
  expect(await page.evaluate(() => (window as any).__testCalls.find((call: any) => call.cmd === 'send_files').args))
    .toMatchObject({ peerId: 'b'.repeat(64), paths: ['D:/文件/项目资料.pdf', 'D:/文件/封面.png'] });
});

test('device list sorts online first then favorites and places filled hearts before status', async ({ page }) => {
  await page.evaluate(() => {
    const base = (window as any).__testState.peers[0];
    (window as any).__testState.peers = [
      { ...base, node_id: 'c'.repeat(64), display_name: 'Offline ordinary', online: false },
      { ...base, node_id: 'd'.repeat(64), display_name: 'Offline favorite', online: false },
      { ...base, display_name: 'Online ordinary' },
      { ...base, node_id: 'e'.repeat(64), display_name: 'Online favorite' },
    ];
  });
  const cards = page.locator('.devices-panel .device-card');
  await expect(cards).toHaveCount(4);
  for (const name of ['Offline favorite', 'Online favorite']) {
    await cards.filter({ hasText: name }).click({ button: 'right' });
    const add = page.getByRole('button', { name: '加入收藏夹' });
    await expect(add.locator('svg')).toHaveCount(0);
    await add.click();
    await page.getByRole('button', { name: '关闭设备详情' }).click();
  }
  await expect(cards.locator('strong')).toHaveText(['Online favorite', 'Online ordinary', 'Offline favorite', 'Offline ordinary']);
  await expect(cards.locator('.device-favorite-mark + .status-dot')).toHaveCount(2);
  expect(await cards.first().locator('.device-favorite-mark svg').evaluate(svg => {
    const style = getComputedStyle(svg);
    return style.fill === style.color;
  })).toBe(true);
  await expect(page.locator('.send-button svg')).toHaveCount(0);
});

test('send selection requires recipient and only displays completed after receipt', async ({ page }) => {
  await expect(page.getByRole('heading', { name: '传递，就在身边' })).toBeVisible();
  const send = page.locator('.send-button'); await expect(send).toBeDisabled();
  await page.getByRole('button', { name: '选择文件', exact: true }).click();
  await expect(page.getByText('2 个文件 · 3.10 MB', { exact: true })).toBeVisible(); await expect(send).toBeDisabled();
  await page.getByRole('button', { name: /Studio Mac/ }).click(); await expect(send).toBeEnabled();
  await page.screenshot({ path: 'docs/screenshots/send-desktop.png', fullPage: true });
  await send.click(); await expect(page.locator('.task-card .badge')).toHaveText('等待接收确认');
  await expect(page.getByText('已完成', { exact: true })).toHaveCount(0);
  await page.evaluate(() => { const t = (window as any).__testState.tasks[0]; t.status = 'transferring'; t.completed_bytes = 0; });
  await expect(page.locator('.task-card .badge')).toHaveText('传输中');
  await page.evaluate(() => { (window as any).__testState.tasks[0].completed_bytes = 1048576; });
  await expect(page.locator('.task-meta')).toContainText('32.26%');
  await expect(page.locator('.transfer-speed')).not.toHaveText('0.00 B/s');
  await expect(page.locator('.transfer-speed')).toHaveText(/\d+\.\d{2} [KMGT]?B\/s/);
  await page.evaluate(() => { const t = (window as any).__testState.tasks[0]; t.status = 'completed'; t.completed_bytes = t.total_bytes; t.events.push('收到接收方的文件落盘回执'); });
  await expect(page.locator('.task-card .badge')).toHaveText('已完成');
  await expect(page.locator('.transfer-speed')).toHaveText('0.00 B/s');
  await expect(page.locator('.task-meta')).toContainText('100.00%');
  await page.screenshot({ path: 'docs/screenshots/transfers-desktop.png', fullPage: true });
});
test('receive request supports destination and explicit approval', async ({ page }) => {
  await page.evaluate(() => { const s = (window as any).__testState; s.tasks.push({ id: 'in-test', peer_id: s.peers[0].node_id, peer_name: 'Studio Mac', direction: 'receive', status: 'waiting', entries: [{ path: '设计稿.pdf', size: 123456, directory: false, modified_ms: 0 }], total_bytes: 123456, completed_bytes: 0, transport: 'quic', error: null, destination: null, events: [], created_at: '2026-09-28T12:00:00Z', verify_hash: false }); });
  await expect(page.getByText('1 个接收请求等待确认')).toBeVisible();
  await page.getByText('1 个接收请求等待确认').click(); await expect(page.getByText('Studio Mac 想发送文件')).toBeVisible();
  await page.getByRole('button', { name: /D:\/Downloads\/NodeSend/ }).click();
  await expect(page.getByRole('button', { name: /D:\/Received/ })).toBeVisible();
  await page.screenshot({ path: 'docs/screenshots/receive-desktop.png', fullPage: true });
  await page.getByRole('checkbox', { name: /信任此设备/ }).check();
  await page.getByRole('button', { name: '确认接收', exact: true }).click();
  await expect(page.getByText('准备好接收了')).toBeVisible();
  const call = await page.evaluate(() => (window as any).__testCalls.find((c: any) => c.cmd === 'decide_incoming_transfer'));
  expect(call.args).toMatchObject({ accepted: true, destination: 'D:/Received', conflict: 'rename', selected: ['设计稿.pdf'], trustDevice: true });
});
test('manual connection reports structured errors', async ({ page }) => {
  await page.getByText('手动发现设备', { exact: true }).click();
  await page.getByPlaceholder('192.168.1.20 或设备主机名（可选 :端口）').fill('192.168.1.99:58082');
  await page.getByRole('button', { name: '验证并添加设备' }).click(); await expect(page.getByRole('alert')).toHaveText('无法连接此地址，请检查端口');
  await page.getByRole('button', { name: '取消' }).click();
  await page.getByRole('button', { name: '本机与设置' }).click(); await expect(page.getByRole('heading', { name: '本机与设置' })).toBeVisible();
});
test('public path asks for confirmation on demand instead of a standing checkbox', async ({ page }) => {
  await page.getByText('手动发现设备', { exact: true }).click();
  const prompt = page.getByRole('alertdialog');
  await expect(page.getByRole('checkbox', { name: '允许本次连接及发送使用公网路径' })).toHaveCount(0);
  await page.getByPlaceholder('192.168.1.20 或设备主机名（可选 :端口）').fill('203.0.113.7:58082');
  await page.getByRole('button', { name: '验证并添加设备' }).click();
  await expect(prompt).toBeVisible();
  await expect(prompt).toContainText('203.0.113.7:58082');
  const firstTry = await page.evaluate(() => (window as any).__testCalls.filter((call: any) => call.cmd === 'add_endpoint').map((call: any) => call.args.allowPublic));
  expect(firstTry).toEqual([false]);
  await page.getByRole('button', { name: '返回修改地址' }).click();
  await expect(prompt).toHaveCount(0);
  await page.getByRole('button', { name: '验证并添加设备' }).click();
  await page.getByRole('button', { name: '允许并使用公网路径' }).click();
  await expect(prompt).toHaveCount(0);
  const attempts = await page.evaluate(() => (window as any).__testCalls.filter((call: any) => call.cmd === 'add_endpoint').map((call: any) => call.args.allowPublic));
  expect(attempts).toEqual([false, false, true]);
  await expect(page.getByText('Public Node', { exact: true })).toBeVisible();
});
test('send panels match in size and the settings card fills the page width', async ({ page }) => {
  await expect(page.locator('.payload-panel')).toBeVisible();
  await expect(page.locator('.devices-panel')).toBeVisible();
  const panels = await page.evaluate(() => {
    const box = (selector: string) => document.querySelector(selector)!.getBoundingClientRect();
    const payload = box('.payload-panel'); const devices = box('.devices-panel');
    return { payload: [Math.round(payload.width), Math.round(payload.height)], devices: [Math.round(devices.width), Math.round(devices.height)] };
  });
  expect(panels.payload).toEqual(panels.devices);
  await page.getByRole('button', { name: '本机与设置' }).click();
  await expect(page.getByRole('heading', { name: '本机与设置' })).toBeVisible();
  const widths = await page.evaluate(() => {
    const pageBox = document.querySelector('.page')!.getBoundingClientRect();
    const card = document.querySelector('.settings-card')!.getBoundingClientRect();
    return { page: Math.round(pageBox.width), card: Math.round(card.width), left: Math.round(card.left - pageBox.left) };
  });
  expect(widths.card).toBe(widths.page);
  expect(widths.left).toBe(0);
});
test('responsive view has no horizontal overflow', async ({ page }) => {
  await page.setViewportSize({ width: 760, height: 900 });
  await expect(page.getByRole('heading', { name: '传递，就在身边' })).toBeVisible();
  const fits = await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth && document.querySelector('main')!.scrollWidth <= document.querySelector('main')!.clientWidth);
  expect(fits).toBe(true); await page.screenshot({ path: 'docs/screenshots/send-compact.png', fullPage: true });
});
test('long selected filenames stay ellipsized with a visible remove button on mobile', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 800 });
  await page.evaluate(() => {
    const native = (window as any).__TAURI_INTERNALS__;
    const original = native.invoke;
    native.invoke = async (cmd: string, args: any) => cmd === 'choose_send_files'
      ? ['/cache/' + '很长的文件名称WithoutSpaces'.repeat(20) + '.pdf'] : original(cmd, args);
  });
  await page.locator('.send-type-buttons').getByRole('button', { name: '文件', exact: true }).click();
  const row = page.locator('.selection-row').first();
  await expect(row.locator('span')).toHaveCSS('text-overflow', 'ellipsis');
  expect(await row.evaluate(element => {
    const text = element.querySelector('span')!;
    const remove = element.querySelector('button')!.getBoundingClientRect();
    return text.scrollWidth > text.clientWidth && remove.right <= window.innerWidth
      && document.querySelector('main')!.scrollWidth <= document.querySelector('main')!.clientWidth;
  })).toBe(true);
  await row.getByRole('button').click();
  await expect(page.locator('.selection-row')).toHaveCount(0);
});
test('Android back follows page history and only arms exit on send', async ({ page }) => {
  const back = () => page.evaluate(() => {
    const registrations = (window as any).__testCalls.filter((call: any) => call.cmd === 'plugin:app|register_listener');
    registrations[registrations.length - 1].args.handler.onmessage({ canGoBack: false });
  });
  await page.getByRole('button', { name: '接收', exact: true }).click();
  await page.getByRole('button', { name: '本机与设置', exact: true }).click();
  await back();
  await expect(page.getByRole('heading', { name: '接收，尽在掌握' })).toBeVisible();
  await expect(page.locator('.back-exit-hint')).toHaveCount(0);
  await back();
  await expect(page.getByRole('heading', { name: '传递，就在身边' })).toBeVisible();
  await expect(page.locator('.back-exit-hint')).toHaveCount(0);
  await back();
  await expect(page.locator('.back-exit-hint')).toBeVisible();
  await page.getByRole('button', { name: '接收', exact: true }).click();
  await expect(page.locator('.back-exit-hint')).toHaveCount(0);
  await back();
  await back();
  await back();
  expect(await page.evaluate(() => (window as any).__testCalls.filter((call: any) => call.cmd === 'plugin:app|exit').length)).toBe(1);
});
test('Android settings show the selected public directory instead of its private staging path', async ({ page }) => {
  await page.evaluate(() => {
    Object.defineProperty(navigator, 'userAgent', { value: 'Android', configurable: true });
    const native = (window as any).__TAURI_INTERNALS__;
    const original = native.invoke;
    native.invoke = async (cmd: string, args: any) => {
      if (cmd === 'describe_receive_directory') return args.root.includes('receive-directories') ? '/Download/NodeSend' : '应用内存储';
      if (cmd === 'choose_receive_directory') return '/data/user/0/com.picdupe.nodesend/files/receive-directories/test';
      return original(cmd, args);
    };
  });
  await page.getByRole('button', { name: '本机与设置', exact: true }).click();
  await page.getByRole('button', { name: '选择', exact: true }).click();
  const directory = page.locator('.settings-card .button-row input');
  await expect(directory).toHaveValue('/Download/NodeSend');
  await expect(directory).toHaveAttribute('readonly', '');
  await page.getByRole('button', { name: '保存设置' }).click();
  expect(await page.evaluate(() => (window as any).__testCalls.find((call: any) => call.cmd === 'set_network_settings')?.args.directory))
    .toBe('/data/user/0/com.picdupe.nodesend/files/receive-directories/test');
});
test('desktop surface suppresses browser context menus and visible scrollbars', async ({ page }) => {
  const contextMenuPrevented = await page.evaluate(() => {
    let prevented = false;
    document.addEventListener('contextmenu', event => { prevented = event.defaultPrevented; }, { once: true });
    document.body.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    return prevented;
  });
  expect(contextMenuPrevented).toBe(true);
  const scrollbarHidden = await page.evaluate(() => {
    const root = document.documentElement;
    return getComputedStyle(root).scrollbarWidth === 'none';
  });
  expect(scrollbarHidden).toBe(true);
});
test('native file browser opens a peer share while HTTP sharing remains off', async ({ page }) => {
  await page.getByRole('button', { name: '共享中心' }).click();
  await page.getByRole('button', { name: '连接', exact: true }).click();
  await page.getByRole('button', { name: '协议', exact: true }).click();
  await page.getByRole('option', { name: 'NodeSend 原生', exact: true }).click();
  await page.getByRole('button', { name: '设备', exact: true }).click();
  await page.getByRole('option', { name: 'Studio Mac', exact: true }).click();
  await page.getByRole('button', { name: '连接并打开' }).click();
  await expect(page.getByRole('button', { name: 'Shared' })).toBeVisible();
  await page.getByRole('button', { name: 'Shared' }).click();
  await expect(page.getByRole('button', { name: 'hello.txt' })).toBeVisible();
  await expect(page.getByText('开启 HTTP 共享')).toHaveCount(0);
  await page.getByRole('checkbox', { name: '选择 hello.txt' }).check();
  await expect(page.locator('.browser-detail')).toContainText('17.00 B');
  const httpCalls = await page.evaluate(() => (window as any).__testCalls.filter((call: any) => call.cmd === 'set_http_config'));
  expect(httpCalls).toHaveLength(0);
});
