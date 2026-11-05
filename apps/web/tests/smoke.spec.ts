import { expect, test } from '@playwright/test';

test('landing links to the workspace', async ({ page }) => {
	await page.goto('/');
	await page.getByRole('link', { name: /open the workspace/i }).click();
	await expect(page).toHaveURL(/\/app$/);
});

test('text → canvas → layouts → fidelity → copy', async ({ page, context, browserName }) => {
	if (browserName === 'chromium') await context.grantPermissions(['clipboard-read', 'clipboard-write']);
	await page.goto('/app');

	// Text-first: sample DBML parsed by the real core (WASM) and rendered.
	const nodes = page.locator('.svelte-flow__node-table');
	await expect(nodes).toHaveCount(2);
	await expect(page.getByRole('button', { name: /round-trip 100% lossless/ })).toBeVisible();

	// Typing a new table updates the canvas (debounced autosync).
	await page.getByRole('textbox', { name: 'dbml source' }).click();
	await page.keyboard.press('ControlOrMeta+End');
	await page.keyboard.type('\nTable tags {\n  id integer [pk]\n}\n');
	await expect(nodes).toHaveCount(3);

	// Fidelity panel shows the real struct (3 tables kept of 3).
	await page.getByRole('button', { name: /round-trip/ }).click();
	await expect(page.getByRole('complementary', { name: 'Fidelity report' })).toContainText('3 / 3');

	// Canvas-first: generated DBML is read-only and mirrors the same state.
	await page.getByRole('tab', { name: /Canvas-first/ }).click();
	await expect(nodes).toHaveCount(3);
	await expect(page.getByRole('textbox', { name: 'dbml source' })).toContainText('Table tags');
	await page.getByRole('button', { name: '＋ Table' }).click();
	await expect(nodes).toHaveCount(4);
	await expect(page.getByRole('textbox', { name: 'dbml source' })).toContainText('table_4');

	// Explorer: sidebar lists every table; hiding one removes its node.
	await page.getByRole('tab', { name: /Explorer/ }).click();
	await expect(page.getByLabel('Schema explorer').getByRole('checkbox')).toHaveCount(4);
	await page.getByLabel('Schema explorer').getByRole('checkbox').first().uncheck();
	await expect(nodes).toHaveCount(3);

	// Copy-to-clipboard: Export → SQL.
	await page.getByRole('button', { name: /Export/ }).click();
	await page.getByRole('menuitem', { name: 'Copy as SQL' }).click();
	await expect(page.getByRole('button', { name: /Copied sql/ })).toBeVisible();
	if (browserName === 'chromium') {
		expect(await page.evaluate(() => navigator.clipboard.readText())).toContain('CREATE TABLE');
	}
});

test('projects persist in the browser across a reload', async ({ page }) => {
	await page.goto('/app');
	const nodes = page.locator('.svelte-flow__node-table');
	await expect(nodes).toHaveCount(2);
	await page.getByRole('textbox', { name: 'dbml source' }).click();
	await page.keyboard.press('ControlOrMeta+End');
	await page.keyboard.type('\nTable kept {\n  id integer [pk]\n}\n');
	await expect(nodes).toHaveCount(3);
	await expect(page.getByText('Saved locally')).toBeVisible();

	await page.reload();
	await expect(nodes).toHaveCount(3);
	await expect(page.getByRole('textbox', { name: 'dbml source' })).toContainText('Table kept');
});

test('import a SQL file as a new project, download it as Prisma', async ({ page }) => {
	await page.goto('/app');
	const nodes = page.locator('.svelte-flow__node-table');
	await expect(nodes).toHaveCount(2);

	await page.getByLabel('Import schema file').setInputFiles({
		name: 'billing.sql',
		mimeType: 'text/plain',
		buffer: Buffer.from(
			'CREATE TABLE plans (id int PRIMARY KEY);\n' +
				'CREATE TABLE subs (id int PRIMARY KEY, plan_id int REFERENCES plans(id) ON DELETE CASCADE);\n' +
				'CREATE TABLE invoices (id int PRIMARY KEY);\n'
		)
	});
	await expect(page.getByRole('status')).toContainText('Imported billing.sql');
	await expect(nodes).toHaveCount(3);
	await expect(page.getByRole('button', { name: 'Projects: billing' })).toBeVisible();

	// both projects are listed; switching back restores the sample
	await page.getByRole('button', { name: 'Projects: billing' }).click();
	await expect(page.getByRole('menu', { name: 'Projects' }).getByRole('menuitem', { name: /shop-demo/ })).toBeVisible();
	await page.getByRole('menuitem', { name: /shop-demo/ }).click();
	await expect(nodes).toHaveCount(2);
	await page.getByRole('button', { name: 'Projects: shop-demo' }).click();
	await page.getByRole('menuitem', { name: /billing/ }).click();
	await expect(nodes).toHaveCount(3);

	await page.getByRole('button', { name: /Export/ }).click();
	const [file] = await Promise.all([
		page.waitForEvent('download'),
		page.getByRole('menuitem', { name: 'Download Prisma' }).click()
	]);
	expect(file.suggestedFilename()).toBe('billing.prisma');
	const text = await (await file.createReadStream()).toArray().then((c) => Buffer.concat(c).toString());
	expect(text).toContain('model plans');
	expect(text).toContain('onDelete: Cascade');
});

test('new project and delete', async ({ page }) => {
	await page.goto('/app');
	const nodes = page.locator('.svelte-flow__node-table');
	await expect(nodes).toHaveCount(2);
	await page.getByRole('button', { name: /Projects:/ }).click();
	await page.getByRole('menuitem', { name: /New project/ }).click();
	await expect(page.getByRole('button', { name: 'Projects: Untitled' })).toBeVisible();
	await expect(nodes).toHaveCount(1);

	page.once('dialog', (d) => d.accept());
	await page.getByRole('button', { name: 'Projects: Untitled' }).click();
	await page.getByRole('menuitem', { name: /Delete/ }).click();
	await expect(page.getByRole('button', { name: 'Projects: shop-demo' })).toBeVisible();
	await expect(nodes).toHaveCount(2);
});

test('sign-in gates import and download when it is enabled', async ({ page }) => {
	await page.route('**/auth/me', (r) => r.fulfill({ json: { enabled: true, user: null } }));
	await page.route('**/auth/login**', (r) => r.fulfill({ contentType: 'text/html', body: '<p>gatehouse</p>' }));
	await page.goto('/app');
	await expect(page.locator('.svelte-flow__node-table')).toHaveCount(2);
	await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible();

	// copy stays free; download asks for sign-in and redirects to /auth/login
	await page.getByRole('button', { name: /Export/ }).click();
	await expect(page.getByRole('menuitem', { name: /Copy as SQL/ })).toBeVisible();
	await page.getByRole('menuitem', { name: /Download Prisma/ }).click();
	await expect(page.getByRole('status')).toContainText('Sign in with CodeCora ID');
	await page.waitForURL(/\/auth\/login\?return_to=%2Fapp$/);
});

test('a signed-in user can download, and sees Sign out', async ({ page }) => {
	await page.route('**/auth/me', (r) =>
		r.fulfill({ json: { enabled: true, user: { sub: 'u1', email: 'ana@example.com' } } })
	);
	await page.goto('/app');
	await expect(page.locator('.svelte-flow__node-table')).toHaveCount(2);
	await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible();
	await expect(page.getByText('ana@example.com')).toBeVisible();
	await page.getByRole('button', { name: /Export/ }).click();
	const [file] = await Promise.all([
		page.waitForEvent('download'),
		page.getByRole('menuitem', { name: 'Download DBML' }).click()
	]);
	expect(file.suggestedFilename()).toBe('shop-demo.dbml');
});

test.describe('phone', () => {
	test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

	test('one pane at a time, switched from the bottom bar, without sideways scroll', async ({ page }) => {
		await page.goto('/app');
		const editor = page.getByRole('textbox', { name: 'dbml source' });
		const nodes = page.locator('.svelte-flow__node-table');
		const panes = page.getByRole('tablist', { name: 'Pane' });
		const overflow = () => page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);

		// text-first opens on the editor; the diagram is one tap away and reflects the edit
		await expect(editor).toBeVisible();
		await expect(nodes).toHaveCount(0);
		await editor.click();
		await page.keyboard.press('ControlOrMeta+End');
		await page.keyboard.type('\nTable tags {\n  id integer [pk]\n}\n');
		expect(await overflow()).toBeLessThanOrEqual(0);
		await panes.getByRole('tab', { name: 'Diagram' }).click();
		await expect(nodes).toHaveCount(3);
		await expect(editor).toHaveCount(0);
		expect(await overflow()).toBeLessThanOrEqual(0);

		// explorer: picking a table jumps to the diagram, focused on it
		await page.getByRole('tab', { name: 'Explorer' }).click();
		await expect(panes.getByRole('tab', { name: 'Tables' })).toHaveAttribute('aria-selected', 'true');
		await page.getByLabel('Schema explorer').getByRole('button', { name: 'users' }).click();
		await expect(panes.getByRole('tab', { name: 'Diagram' })).toHaveAttribute('aria-selected', 'true');
		await expect(page.getByText('Focus:')).toBeVisible();
		expect(await overflow()).toBeLessThanOrEqual(0);
	});
});
