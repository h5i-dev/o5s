import {test} from 'node:test';
import assert from 'node:assert/strict';
import {handle} from './worker.mjs';
function fixtures() {
  const calls=[];
  const binding={
    create:async(...args)=>{calls.push(['create',...args]);return {name:args[0],remote:'https://git.example/repo',token:'secret'};},
    get:repo=>({
      info:async()=>{calls.push(['info',repo]);return {name:repo};},
      fork:async(...args)=>{calls.push(['fork',repo,...args]);return {name:args[0]};},
      createToken:async(...args)=>{calls.push(['token',repo,...args]);return {plaintext:'secret',expiresAt:'date'};},
      [Symbol.dispose]:()=>calls.push(['dispose',repo]),
    }),
  };
  const fetcher=async req=>{
    calls.push(['fetch',req.url,req.headers.get('authorization')]);
    return new Response(JSON.stringify({actor:{tenant:7,user:2},repositories:['demo']}),{status:req.headers.get('authorization')==='Bearer valid'?200:401});
  };
  const env={O5S_ORIGIN:'https://native.example',ARTIFACTS:binding};
  return {env,fetcher,calls};
}
const request=body=>new Request('https://edge.example/api/artifacts',{method:'POST',headers:{authorization:'Bearer valid'},body:JSON.stringify(body)});
test('tenant-scoped create, capability disposal and short-lived write token',async()=>{
  const {env,fetcher,calls}=fixtures();
  assert.equal((await handle(request({repo:'demo',action:'create'}),env,fetcher)).status,200);
  assert.deepEqual(calls.at(-1),['create','7-demo',{setDefaultBranch:'main'}]);
  assert.equal((await handle(request({repo:'demo',action:'fork',fork:'agent1'}),env,fetcher)).status,200);
  assert.deepEqual(calls.at(-2),['fork','7-demo','7-demo-agent1',{setDefaultBranch:'main'}]);
  assert.deepEqual(calls.at(-1),['dispose','7-demo']);
  await handle(request({repo:'demo',action:'token'}),env,fetcher);
  assert.deepEqual(calls.at(-2),['token','7-demo','write',300]);
  assert.equal((await handle(new Request('https://edge.example/api/artifacts?repo=demo',{headers:{authorization:'Bearer valid'}}),env,fetcher)).status,200);
});
test('unauthenticated and foreign repositories never reach Artifacts',async()=>{
  const {env,fetcher,calls}=fixtures();
  assert.equal((await handle(new Request('https://edge.example/api/artifacts',{method:'POST',body:'{}'}),env,fetcher)).status,401);
  assert.equal((await handle(request({repo:'other',action:'token'}),env,fetcher)).status,404);
  assert.equal((await handle(request({repo:'demo',action:'fork',fork:'../escape'}),env,fetcher)).status,400);
  assert.equal(calls.some(c=>c[0]!=='fetch'),false);
});
test('ordinary UI/API requests preserve authorization and query',async()=>{
  const {env,fetcher,calls}=fixtures();
  await handle(new Request('https://edge.example/api/state?repo=demo',{headers:{authorization:'Bearer valid'}}),env,fetcher);
  assert.deepEqual(calls[0],['fetch','https://native.example/api/state?repo=demo','Bearer valid']);
});
