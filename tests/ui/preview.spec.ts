import { test, expect } from '@playwright/test';

test('browser preview renders main workflows without native IPC', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto('/');
  await expect(page.getByText('界面预览 · 示例数据')).toBeVisible();
  await expect(page.getByRole('button', { name: '选择文件', exact: true })).toBeVisible();
  await expect(page.getByText('示例笔记本', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: '选择文件', exact: true }).click();
  await expect(page.getByText('2 个文件 · 3.10 MB', { exact: true })).toBeVisible();
  await page.locator('nav button').filter({ hasText: '接收' }).click();
  await expect(page.getByRole('button', { name: '确认接收', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '共享中心', exact: true }).click();
  await expect(page.getByRole('heading', { name: '文件浏览器' })).toBeVisible();
  await expect(page.getByText('设计稿.pdf', { exact: true }).first()).toBeVisible();
  await page.getByRole('button', { name: '本机与设置', exact: true }).click();
  await expect(page.getByRole('button', { name: '保存设置' })).toBeEnabled();
  expect(errors).toEqual([]);
});
