// Path-inserted form for the /mcp resource; see server/mcp.ts.
import type { Env } from '../../../server/auth';
import { handleAuthorizationServerMetadata } from '../../../server/mcp';

export const onRequestGet = (ctx: { request: Request; env: Env }) => handleAuthorizationServerMetadata(ctx.request, ctx.env);
