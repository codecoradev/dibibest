// RFC 9728 metadata for the /mcp resource (path-suffixed form); see server/mcp.ts.
import type { Env } from '../../../server/auth';
import { handleProtectedResource } from '../../../server/mcp';

export const onRequestGet = (ctx: { request: Request; env: Env }) => handleProtectedResource(ctx.request, ctx.env);
