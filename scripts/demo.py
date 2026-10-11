#!/usr/bin/env python3
"""Exercise the real authenticated HTTP API; never performs a remote push."""
import json, os, pathlib, subprocess, time, urllib.request
ROOT = pathlib.Path(__file__).resolve().parent.parent
ORIGIN = os.environ.get('O5S_URL', 'http://127.0.0.1:3000')
secret = os.environ.get('H5I_APP_SECRET')
if not secret:
    secret = (pathlib.Path(os.environ.get('O5S_DATA', str(ROOT / '.o5s'))) / 'secret').read_text().strip()
exe = pathlib.Path(os.environ.get('CARGO_TARGET_DIR', str(ROOT / 'target'))) / 'debug' / 'o5s-server'
env = dict(os.environ, H5I_APP_SECRET=secret)
tokens = {u: subprocess.check_output([str(exe), 'token', '1', str(u)],env=env,text=True).strip() for u in [1,2,3,4]}
repo = 'demo-' + str(time.time_ns())
def api(user, route, body=None):
    req = urllib.request.Request(ORIGIN + route, data=None if body is None else json.dumps(body).encode(), headers={'Authorization':'Bearer '+tokens[user], 'Content-Type':'application/json'})
    with urllib.request.urlopen(req) as response: return json.load(response)
def cmd(user, **body): return api(user, '/api/command',dict(repo=repo,**body))
def ready(p):
    cmd(3,cmd='approve',patch=p)
    cmd(4,cmd='check',patch=p,passed=True)
def propose(user,intent,deps,text):
    return cmd(user,cmd='propose',intent=intent,dependencies=deps,edits=[{'path':'src/message.txt','content':text}])['patch']
def merge(channel,p): cmd(3,cmd='merge',channel=channel,patch=p)
def view(channel): return api(1,f'/api/state?repo={repo}&channel={channel}')['view']
cmd(1,cmd='create')
base=propose(1,'Establish a shared base',[],'hello\n');ready(base);merge('main',base)
cmd(1,cmd='fork',source='main',channel='reverse')
a=propose(1,'Agent Alice adds a greeting',[base],'hello from Alice\n')
b=propose(2,'Agent Bob adds a greeting',[base],'hello from Bob\n')
for p in [a,b]: ready(p)
for p in [a,b]: merge('main',p)
for p in [b,a]: merge('reverse',p)
assert view('main')==view('reverse')
assert len(view('main')['conflicts']['src/message.txt'])==2
print('Two delivery orders preserve the same two conflicting values.')
resolution=propose(1,'Combine both agents with an explicit resolution',[a,b],'hello from Alice and Bob\n')
ready(resolution)
for c in ['main','reverse']: merge(c,resolution)
assert view('main')==view('reverse') and not view('main')['conflicts']
export=api(1,'/api/export',{'repo':repo,'channel':'main'})
text=subprocess.check_output(['git','--git-dir',export['path'],'show','main:src/message.txt'],text=True)
assert text=='hello from Alice and Bob\n'
assert api(1,'/api/export',{'repo':repo,'channel':'main'})['commit']==export['commit']
workspace=api(1,'/api/worktree',{'repo':repo,'channel':'main'})
assert pathlib.Path(workspace['path'],'src/message.txt').read_text()==text
assert not subprocess.run(['git','-C',workspace['path'],'symbolic-ref','-q','HEAD'],capture_output=True).returncode==0
linebase=propose(1,'Three lines for parallel range editing',[resolution],'one\ntwo\nthree\n')
ready(linebase);merge('main',linebase)
line_ids=[]
for user,start,end,replacement in [(1,0,1,'ONE\n'),(2,2,3,'THREE\n')]:
    p=cmd(user,cmd='propose_lines',intent='Parallel line edit',dependencies=[],edits=[{'path':'src/message.txt','base':linebase,'start':start,'end':end,'content':replacement}])['patch']
    ready(p);line_ids.append(p)
cmd(3,cmd='merge_batch',channel='main',patches=line_ids)
assert view('main')['files']['src/message.txt']=='ONE\ntwo\nTHREE\n'
assert not view('main')['conflicts']
print('Disjoint line edits merge automatically on their immutable shared base.')
newworkspace=api(1,'/api/worktree',{'repo':repo,'channel':'main'})
assert newworkspace['path']!=workspace['path']
assert pathlib.Path(workspace['path'],'src/message.txt').read_text()==text
print('Detached workspaces retain their own snapshot as the channel advances.')
print(json.dumps({'repository':repo,'git_export':export,'ui':ORIGIN},indent=2))
