import { expect, test } from '@playwright/test';
import { readFile } from 'node:fs/promises';

test('landing: live demo runs the real core, and links to the workspace', async ({ page }) => {
	await page.goto('/');
	const out = page.getByLabel('Converted output');
	await expect(out).toContainText('CREATE TABLE users');
	await page.getByRole('tab', { name: 'Prisma' }).click();
	await expect(out).toContainText('model users');
	await page.getByLabel('DBML input').fill('Table tags {\n  id integer [pk]\n}\n');
	await expect(out).toContainText('model tags');
	await page.getByRole('link', { name: /open the workspace/i }).first().click();
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
	await page.keyboard.insertText('\nTable tags {\n  id integer [pk]\n}\n');
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
	await page.keyboard.insertText('\nTable kept {\n  id integer [pk]\n}\n');
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

test('sign-in gates import, copy and download when it is enabled', async ({ page }) => {
	await page.route('**/auth/me', (r) => r.fulfill({ json: { enabled: true, user: null } }));
	await page.route('**/auth/login**', (r) => r.fulfill({ contentType: 'text/html', body: '<p>gatehouse</p>' }));
	await page.goto('/app');
	await expect(page.locator('.svelte-flow__node-table')).toHaveCount(2);
	await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible();

	// copy and download both ask for sign-in and redirect to /auth/login
	await page.getByRole('button', { name: /Export/ }).click();
	await expect(page.getByRole('menuitem', { name: /Copy as SQL/ })).toContainText('sign in');
	await page.getByRole('menuitem', { name: /Copy as SQL/ }).click();
	await expect(page.getByRole('status')).toContainText('Sign in with CodeCora ID to copy');
	await page.waitForURL(/\/auth\/login\?return_to=%2Fapp$/);
	await page.goto('/app');
	await expect(page.locator('.svelte-flow__node-table')).toHaveCount(2);
	await page.getByRole('button', { name: /Export/ }).click();
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

	// the diagram as an image (#60): PNG and SVG
	for (const [item, name, magic] of [
		['Download Image (PNG, dark)', 'shop-demo.png', '\x89PNG'],
		['Download Image (PNG, light)', 'shop-demo-light.png', '\x89PNG'],
		['Download Image (SVG)', 'shop-demo.svg', '<svg']
	] as const) {
		await page.getByRole('button', { name: /Export/ }).click();
		const [img] = await Promise.all([page.waitForEvent('download'), page.getByRole('menuitem', { name: item }).click()]);
		expect(img.suggestedFilename()).toBe(name);
		const bytes = await readFile((await img.path())!);
		expect(bytes.subarray(0, magic.length).toString('latin1')).toBe(magic);
		if (name.endsWith('.svg')) expect(bytes.toString()).toContain('>orders<');
	}
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
		await page.keyboard.insertText('\nTable tags {\n  id integer [pk]\n}\n');
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

test('dbdiagram sample with optional refs: no error, three refs, no overlapping tables (#75)', async ({ page }) => {
	await page.goto('/app');
	const nodes = page.locator('.svelte-flow__node-table');
	await expect(nodes).toHaveCount(2);
	const editor = page.getByRole('textbox', { name: 'dbml source' });
	await editor.click();
	await page.keyboard.press('ControlOrMeta+A');
	await page.keyboard.press('Delete');
	await page.keyboard.insertText(
		[
			'Table follows {',
			'  following_user_id integer [not null]',
			'  followed_user_id integer [not null]',
			'  created_at timestamp',
			'}',
			'',
			'Table users {',
			'  id integer [primary key]',
			'  username varchar',
			'  role varchar',
			'  created_at timestamp',
			'}',
			'',
			'Table posts {',
			'  id integer [primary key]',
			'  title varchar',
			"  body text [note: 'Content of the post']",
			'  user_id integer [not null]',
			'  status varchar',
			'  created_at timestamp',
			'}',
			'',
			'Ref user_posts: posts.user_id ?> users.id // many-to-one',
			'',
			'Ref: users.id <? follows.following_user_id',
			'',
			'Ref: users.id <? follows.followed_user_id',
			''
		].join('\n')
	);
	await expect(nodes).toHaveCount(3);
	await expect(page.getByText('3 tables · 3 refs').first()).toBeVisible();
	await expect(page.getByText(/parse error/)).toHaveCount(0);
	await expect(page.locator('.svelte-flow__edge')).toHaveCount(3);
	// no two tables overlap on the canvas
	const boxes = await nodes.evaluateAll((els) => els.map((e) => e.getBoundingClientRect().toJSON()));
	for (let i = 0; i < boxes.length; i++)
		for (let j = i + 1; j < boxes.length; j++) {
			const a = boxes[i],
				b = boxes[j];
			const overlap = a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
			expect(overlap, `tables ${i} and ${j} overlap`).toBe(false);
		}
});

test('a column or table declared twice shows a parse error instead of crashing the page', async ({ page }) => {
	const crashes: string[] = [];
	page.on('pageerror', (e) => crashes.push(e.message));
	await page.goto('/app');
	const nodes = page.locator('.svelte-flow__node-table');
	await expect(nodes).toHaveCount(2);
	await page.getByRole('textbox', { name: 'dbml source' }).click();
	await page.keyboard.press('ControlOrMeta+End');
	await page.keyboard.insertText('\nTable z {\n  id int\n  id int\n}\n');
	await expect(page.getByText(/duplicate column `z\.id`/).first()).toBeVisible();
	await expect(page.getByText('Internal Error')).toHaveCount(0);
	await expect(nodes).toHaveCount(2);
	expect(crashes).toEqual([]);
});

test('a project saved with duplicate columns by an older build opens instead of 500', async ({ page }) => {
	const crashes: string[] = [];
	page.on('pageerror', (e) => crashes.push(e.message));
	await page.goto('/app');
	await expect(page.locator('.svelte-flow__node-table')).toHaveCount(2);
	// what an older build stored: the pasted copy left z.id twice in text and diagram
	await page.evaluate(
		() =>
			new Promise<void>((resolve, reject) => {
				const col = (name: string) => ({ name, type: 'int', isPk: false, isFk: false, isUnique: false, isNullable: true });
				const req = indexedDB.open('dibibest', 1);
				req.onsuccess = () => {
					const tx = req.result.transaction('projects', 'readwrite');
					tx.objectStore('projects').put({
						id: 'broken',
						name: 'broken',
						format: 'dbml',
						text: 'Table z {\n  id int\n  id int\n}\n',
						erd: {
							version: 1,
							nodes: [{ id: 'z', name: 'z', position: { x: 0, y: 0 }, columns: [col('id'), col('id')] }],
							edges: [],
							meta: { layout: null }
						},
						updatedAt: Date.now() + 1000
					});
					tx.oncomplete = () => {
						localStorage.setItem('dibibest:last-project', 'broken');
						resolve();
					};
					tx.onerror = () => reject(tx.error);
				};
				req.onerror = () => reject(req.error);
			})
	);
	await page.reload();
	await expect(page.getByText(/duplicate column `z\.id`/).first()).toBeVisible();
	await expect(page.getByText('Internal Error')).toHaveCount(0);
	await expect(page.locator('.svelte-flow__node-table')).toHaveCount(1);
	expect(crashes).toEqual([]);
});

test('header popups close on a click outside, on Escape, and when another opens', async ({ page }) => {
	await page.goto('/app');
	await expect(page.locator('.svelte-flow__node-table')).toHaveCount(2);
	const projects = page.getByRole('menu', { name: 'Projects' });
	const exportMenu = page.getByRole('menuitem', { name: /Copy as/ }).first();
	const fidelity = page.getByRole('complementary', { name: 'Fidelity report' });

	// click outside (the canvas) closes the project menu
	await page.getByRole('button', { name: /^Projects:/ }).click();
	await expect(projects).toBeVisible();
	await page.locator('.svelte-flow__pane').click({ position: { x: 5, y: 5 } });
	await expect(projects).toHaveCount(0);

	// Escape closes the export menu
	await page.getByRole('button', { name: 'Export ▾' }).click();
	await expect(exportMenu).toBeVisible();
	await page.keyboard.press('Escape');
	await expect(exportMenu).toHaveCount(0);

	// clicks inside the fidelity panel keep it open; opening Export closes it
	await page.getByRole('button', { name: /round-trip/ }).click();
	await expect(fidelity).toBeVisible();
	await fidelity.click();
	await expect(fidelity).toBeVisible();
	await page.getByRole('button', { name: 'Export ▾' }).click();
	await expect(fidelity).toHaveCount(0);
	await expect(exportMenu).toBeVisible();

	// the toggle still closes its own popup
	await page.getByRole('button', { name: 'Export ▾' }).click();
	await expect(exportMenu).toHaveCount(0);
});

test('laptop width: the header stays on one row and tall tables do not overlap', async ({ page }) => {
	await page.setViewportSize({ width: 1325, height: 663 });
	await page.route('**/auth/me', (r) =>
		r.fulfill({ json: { enabled: true, user: { sub: 'u1', email: 'ana@example.com', name: 'Ana Maria Longname' } } })
	);
	await page.goto('/app');
	await expect(page.locator('.svelte-flow__node-table')).toHaveCount(2);

	// a 20-column table first, then tables that must land below it, not on it
	const cols = Array.from({ length: 20 }, (_, i) => `  col_${i} varchar`).join('\n');
	const src = `Table big_one {\n  id uuid [pk]\n${cols}\n}\nTable b {\n  id uuid [pk]\n}\nTable c {\n  id uuid [pk]\n}\nTable d {\n  id uuid [pk]\n}\n`;
	await page.getByRole('textbox', { name: 'dbml source' }).click();
	await page.keyboard.press('ControlOrMeta+A');
	await page.keyboard.press('Delete');
	await page.keyboard.insertText(src);
	const nodes = page.locator('.svelte-flow__node-table');
	await expect(nodes).toHaveCount(4);

	// header: one 48px row, no control wraps onto a second line
	const bar = await page.locator('header.topbar').boundingBox();
	expect(bar!.height).toBeLessThanOrEqual(49);
	for (const loc of [page.locator('.chip'), page.locator('.btn-primary'), page.locator('.seg button'), page.locator('.project-name')]) {
		for (const b of await loc.evaluateAll((els) => els.map((e) => e.getBoundingClientRect().height))) expect(b).toBeLessThanOrEqual(40);
	}
	// nothing is clipped off the right edge
	const right = await page.locator('header.topbar').evaluate((el) => el.scrollWidth - el.clientWidth);
	expect(right).toBeLessThanOrEqual(0);

	// the whole schema is fitted on screen once pasted
	await page.waitForTimeout(600);
	const boxes = await nodes.evaluateAll((els) => els.map((e) => e.getBoundingClientRect().toJSON()));
	const pane = await page.locator('.svelte-flow').boundingBox();
	for (const b of boxes) {
		expect(b.left).toBeGreaterThanOrEqual(pane!.x - 1);
		expect(b.right).toBeLessThanOrEqual(pane!.x + pane!.width + 1);
		expect(b.top).toBeGreaterThanOrEqual(pane!.y - 1);
		expect(b.bottom).toBeLessThanOrEqual(pane!.y + pane!.height + 1);
	}
	for (let i = 0; i < boxes.length; i++)
		for (let j = i + 1; j < boxes.length; j++) {
			const a = boxes[i],
				b = boxes[j];
			expect(a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom, `${i} overlaps ${j}`).toBe(false);
		}
});

test('editor: closes brackets, indents, Tab accepts a suggestion, Format tidies, Escape then Tab leaves', async ({ page }) => {
	await page.goto('/app');
	const nodes = page.locator('.svelte-flow__node-table');
	await expect(nodes).toHaveCount(2);
	const editor = page.getByRole('textbox', { name: 'dbml source' });
	const lines = () => page.locator('.cm-content .cm-line').evaluateAll((els) => els.map((e) => e.textContent ?? ''));
	const popup = page.locator('.cm-tooltip-autocomplete');

	await editor.click();
	await page.keyboard.press('ControlOrMeta+A');
	await page.keyboard.press('Delete');

	// "{" brings its "}"; Enter opens an indented line between them
	await page.keyboard.type('Table t {');
	await expect.poll(lines).toEqual(['Table t {}']);
	await page.keyboard.press('Enter');
	await expect.poll(lines).toEqual(['Table t {', '  ', '}']);

	// a suggestion for the type, accepted with Tab: "integ" becomes "integer"
	await page.keyboard.type('id integ');
	await expect(popup).toBeVisible();
	await page.keyboard.press('Tab');
	await expect.poll(async () => (await lines())[1]).toBe('  id integer');

	// with no suggestion open, Tab indents and Shift+Tab takes it back
	await page.keyboard.press('Enter');
	await page.keyboard.press('Tab');
	await expect.poll(async () => (await lines())[2]).toBe('    ');
	await page.keyboard.press('Shift+Tab');
	await expect.poll(async () => (await lines())[2]).toBe('  ');

	// the enum-or-type list answers a prefix; "]" typed over the auto-closed one adds no second
	await page.keyboard.type('name varc');
	await expect(popup).toBeVisible();
	await page.keyboard.press('Tab');
	await page.keyboard.type(' [pk]');
	await expect.poll(async () => (await lines())[2]).toBe('  name varchar [pk]');
	await expect(page.locator('article[aria-label="t"]')).toBeVisible();
	await expect(page.getByText('✓ valid DBML')).toBeVisible();

	// Format: whitespace only (indent, trailing spaces, blank lines), keeps the schema
	await page.keyboard.press('ControlOrMeta+A');
	await page.keyboard.press('Delete');
	await page.keyboard.insertText('\n\nTable m {\nid int   \n\n\n   x text\n  }\n');
	await expect(page.locator('article[aria-label="m"]')).toBeVisible();
	const tidy = page.getByRole('button', { name: 'Format' });
	await tidy.click();
	await expect.poll(async () => (await lines()).join('\n')).toBe('Table m {\n  id int\n\n  x text\n}\n');
	await expect(page.getByRole('button', { name: 'Formatted ✓' })).toBeVisible();
	await expect(page.locator('article[aria-label="m"]')).toBeVisible();
	await page.waitForTimeout(1500);
	await tidy.click();
	await expect(page.getByRole('button', { name: 'Already tidy' })).toBeVisible();

	// keyboard users are not trapped: Escape, then Tab moves focus out of the editor
	await editor.click();
	await page.keyboard.press('Escape');
	await page.keyboard.press('Tab');
	expect(await page.evaluate(() => !!document.activeElement?.closest('.cm-editor'))).toBe(false);

	// ...and backwards too: Shift fires a keydown of its own first, which must not cancel the Escape
	await editor.click();
	await page.keyboard.press('Escape');
	await page.keyboard.press('Shift+Tab');
	expect(await page.evaluate(() => !!document.activeElement?.closest('.cm-editor'))).toBe(false);
});
