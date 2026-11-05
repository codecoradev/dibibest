// GET /auth/callback — logic in server/handlers.ts (unit-tested there).
import { handleCallback } from '../../server/handlers';
import type { Env } from '../../server/auth';

export const onRequestGet = (ctx: { request: Request; env: Env }) => handleCallback(ctx.request, ctx.env);
