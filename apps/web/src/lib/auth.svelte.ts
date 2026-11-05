// Sign-in state from the Pages Function `/auth/me` (epic #52). Where that route
// does not exist (vite dev, a plain static host) sign-in is simply off and
// nothing is gated: the playground never depends on a server.
export type User = { sub: string; email: string; name?: string };

class Auth {
	/** True when the deployment has sign-in configured. */
	enabled = $state(false);
	user = $state<User | null>(null);
	loaded = $state(false);

	async load() {
		try {
			const res = await fetch('/auth/me', { headers: { accept: 'application/json' }, cache: 'no-store' });
			const ct = res.headers.get('content-type') ?? '';
			if (res.ok && ct.includes('application/json')) {
				const body = (await res.json()) as { enabled?: boolean; user?: User | null };
				this.enabled = body.enabled === true;
				this.user = body.user ?? null;
			}
		} catch {
			/* offline or no Functions: sign-in stays off */
		}
		this.loaded = true;
	}

	/** Import/export need a signed-in user when sign-in is enabled. */
	get mayTransfer() {
		return !this.enabled || this.user !== null;
	}

	signIn(returnTo = '/app') {
		location.assign(`/auth/login?return_to=${encodeURIComponent(returnTo)}`);
	}
}

export const auth = new Auth();
