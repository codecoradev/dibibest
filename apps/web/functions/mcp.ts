// /mcp — remote MCP endpoint; logic in server/mcp.ts (unit-tested there).
import wasm from '../src/lib/wasm/dibibest.wasm';
import type { Env } from '../server/auth';
import { makeCore } from '../server/core';
import { handleMcp } from '../server/mcp';

const core = makeCore(wasm);

export const onRequest = (ctx: { request: Request; env: Env }) => handleMcp(ctx.request, ctx.env, core);
