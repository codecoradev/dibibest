// GET /auth/me — logic in server/handlers.ts (unit-tested there).
import { handleMe } from '../../server/handlers';
import type { Env } from '../../server/auth';

export const onRequestGet = (ctx: { request: Request; env: Env }) => handleMe(ctx.request, ctx.env);
