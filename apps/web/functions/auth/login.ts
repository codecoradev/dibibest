// GET /auth/login — logic in server/handlers.ts (unit-tested there).
import { handleLogin } from '../../server/handlers';
import type { Env } from '../../server/auth';

export const onRequestGet = (ctx: { request: Request; env: Env }) => handleLogin(ctx.request, ctx.env);
