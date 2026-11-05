// RFC 9728 metadata at the root path; see server/mcp.ts.
import type { Env } from '../../server/auth';
import { handleProtectedResource } from '../../server/mcp';

export const onRequestGet = (ctx: { request: Request; env: Env }) => handleProtectedResource(ctx.request, ctx.env);
