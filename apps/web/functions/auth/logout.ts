// POST /auth/logout — logic in server/handlers.ts (unit-tested there).
import { handleLogout } from '../../server/handlers';

export const onRequest = (ctx: { request: Request }) => handleLogout(ctx.request);
