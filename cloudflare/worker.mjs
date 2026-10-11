/** Cloudflare edge adapter. Native axum owns patch admission and persistence.
 * Artifacts owns Git projections/forks; credentials never enter patch records.
 * See https://developers.cloudflare.com/artifacts/api/workers-binding/
 */
const name = s => typeof s === 'string' && /^[a-zA-Z0-9_-]{1,64}$/.test(s);
const json = (value, status = 200) => new Response(JSON.stringify(value), {
  status, headers: {'content-type': 'application/json', 'cache-control': 'no-store'},
});
export class ArtifactsAdapter {
  constructor(binding) { this.binding = binding; }
  async create(repo) { return this.binding.create(repo, {setDefaultBranch: 'main'}); }
  async info(repo) {
    const capability = this.binding.get(repo);
    try { return await capability.info(); }
    finally { capability[Symbol.dispose]?.(); }
  }
  async fork(repo, target) {
    const capability = this.binding.get(repo);
    try { return await capability.fork(target, {setDefaultBranch: 'main'}); }
    finally { capability[Symbol.dispose]?.(); }
  }
  async token(repo) {
    const capability = this.binding.get(repo);
    try { return await capability.createToken('write', 300); }
    finally { capability[Symbol.dispose]?.(); }
  }
}
export async function handle(request, env, fetcher = fetch) {
  const incoming = new URL(request.url);
  let origin;
  try {
    origin = new URL(env.O5S_ORIGIN);
    if (origin.username || origin.password ||
        (origin.protocol !== 'https:' && !['localhost','127.0.0.1'].includes(origin.hostname)))
      throw new Error('invalid origin');
  } catch { return json({error: 'O5S_ORIGIN is not configured'}, 503); }
  origin.pathname = incoming.pathname; origin.search = incoming.search;
  if (!incoming.pathname.startsWith('/api/artifacts')) {
    return fetcher(new Request(origin, request));
  }
  if (incoming.pathname !== '/api/artifacts' || !['GET','POST'].includes(request.method))
    return json({error:'route not found'},404);
  // Resolve identity and tenant membership through the same authenticated API
  // as the local UI. Never use a tenant ID supplied in the request body.
  const identityURL = new URL('/api/repositories', origin);
  const identity = await fetcher(new Request(identityURL, {
    headers: {authorization: request.headers.get('authorization') || ''},
  }));
  if (!identity.ok) return json({error:'authentication failed'}, identity.status);
  const {actor, repositories} = await identity.json();
  let body;
  try { body = request.method === 'GET' ? {repo: incoming.searchParams.get('repo'), action:'info'} : await request.json(); }
  catch { return json({error:'invalid JSON'},400); }
  if (!name(body.repo) || !repositories.includes(body.repo))
    return json({error:'repository not found'},404);
  if (!env.ARTIFACTS) return json({error:'Artifacts binding is not configured'},503);
  const adapter = new ArtifactsAdapter(env.ARTIFACTS);
  const scoped = `${actor.tenant}-${body.repo}`;
  try {
    switch (body.action) {
      case 'create': return json(await adapter.create(scoped));
      case 'info': return json(await adapter.info(scoped));
      case 'fork':
        if (!name(body.fork)) return json({error:'invalid fork name'},400);
        return json(await adapter.fork(scoped, `${actor.tenant}-${body.repo}-${body.fork}`));
      case 'token': return json(await adapter.token(scoped));
      default: return json({error:'unknown action'},400);
    }
  } catch { return json({error:'Artifacts operation failed'},502); }
}
export default {fetch(request, env) { return handle(request, env); }};
