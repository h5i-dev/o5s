#!/usr/bin/env python3
"""Seed a demonstration repository through the authenticated HTTP API.

Ten agents work on a small expression parser: their edits to different lines
of the same files merge, two of them collide on one line and leave a graph
conflict, some patches wait for review, one fails its check, and a forked
channel holds an experiment. Files are graph documents, edited the way
`o5s propose` edits them: unchanged lines keep their identities.

    O5S_URL=http://127.0.0.1:3000 H5I_APP_SECRET=... scripts/seed-demo.py

O5S_SERVER is the o5s-server binary that signs tokens (default
target/release/o5s-server, as scripts/dev.sh builds it); O5S_ORG the organization (default 1); O5S_REPO the
repository name (default octo-parser). Prints a token for a viewer, whose
role is read-only.
"""
import json, os, pathlib, subprocess, sys, urllib.error, urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
ORIGIN = os.environ.get('O5S_URL', 'http://127.0.0.1:3000').rstrip('/')
ORG = os.environ.get('O5S_ORG', '1')
REPO = os.environ.get('O5S_REPO', 'octo-parser')
SERVER = os.environ.get('O5S_SERVER', str(ROOT / 'target' / 'release' / 'o5s-server'))
if 'H5I_APP_SECRET' not in os.environ:
    sys.exit('set H5I_APP_SECRET to the server\'s signing key')

OWNER, REVIEWER, CHECKS, VIEWER = 1, 2, 3, 9
AGENTS = list(range(101, 111))
_tokens = {}


def token(user):
    if user not in _tokens:
        _tokens[user] = subprocess.check_output([SERVER, 'token', ORG, str(user)], text=True).strip()
    return _tokens[user]


def api(user, route, body=None):
    request = urllib.request.Request(
        ORIGIN + route, data=None if body is None else json.dumps(body).encode(),
        headers={'Authorization': 'Bearer ' + token(user), 'Content-Type': 'application/json',
                 # Cloudflare refuses urllib's default agent (error 1010).
                 'User-Agent': 'o5s-seed-demo'})
    try:
        with urllib.request.urlopen(request) as response:
            return json.load(response)
    except urllib.error.HTTPError as e:
        sys.exit(f'{route} {body}: {e.code} {e.read().decode()}')


def cmd(actor, **body):
    return api(actor, '/api/command', dict(repo=REPO, **body))


def document(path, patches):
    selection = ','.join(map(str, patches))
    return api(OWNER, f'/api/document?repo={REPO}&channel=main&path={path}&patches={selection}')


def lines(text):
    return [list(line.encode()) for line in text.splitlines(keepends=True)]


def operations(doc, updated):
    """Delete the atoms between the unchanged prefix and suffix and insert the
    new lines there, as `graph_operations` in crates/cli does."""
    atoms = [(layer[0], bytes(next(n['bytes'] for n in doc['nodes'] if n['id'] == layer[0])))
             for layer in doc['layers']]
    target = updated.encode()
    start, left = 0, 0
    while start < len(atoms) and target[left:].startswith(atoms[start][1]):
        left += len(atoms[start][1]); start += 1
    end, right = len(atoms), len(target)
    while end > start and right - len(atoms[end - 1][1]) >= left and target[:right].endswith(atoms[end - 1][1]):
        right -= len(atoms[end - 1][1]); end -= 1
    ops = []
    if start < end:
        ops.append({'kind': 'delete', 'targets': [a[0] for a in atoms[start:end]]})
    if left < right:
        ops.append({'kind': 'insert',
                    'left': 'Start' if start == 0 else {'Node': atoms[start - 1][0]},
                    'right': 'End' if end == len(atoms) else {'Node': atoms[end][0]},
                    'atoms': lines(target[left:right].decode())})
    return ops


def propose(user, intent, snapshot, files):
    """Propose new contents of `files` against the patch set `snapshot`."""
    edits = []
    for path, text in files.items():
        doc = document(path, snapshot) if snapshot else None
        if doc and doc['nodes']:
            edits.append({'path': path, 'operations': operations(doc, text)})
        else:
            edits.append({'path': path, 'operations': [
                {'kind': 'insert', 'left': 'Start', 'right': 'End', 'atoms': lines(text)}]})
    deps = heads(snapshot)
    return cmd(user, cmd='propose_graph', intent=intent, dependencies=deps, edits=edits)['patch']


def heads(snapshot):
    patches = api(OWNER, f'/api/state?repo={REPO}')['repository']['patches']
    covered = {d for p in snapshot for d in patches[str(p)]['dependencies']}
    return [p for p in snapshot if p not in covered]


def review(patch, passed=True):
    cmd(REVIEWER, cmd='approve', patch=patch)
    cmd(CHECKS, cmd='check', patch=patch, passed=passed)


def members(channel='main'):
    return api(OWNER, f'/api/state?repo={REPO}&channel={channel}')['view']['patches']


LEXER = '''//! Tokens of a small arithmetic language.
pub enum Token { Num(i64), Plus, Minus, Star, Slash, LParen, RParen }

pub fn lex(src: &str) -> Vec<Token> {
    let mut out = Vec::new();
    for c in src.chars() {
        match c {
            '+' => out.push(Token::Plus),
            '-' => out.push(Token::Minus),
            '*' => out.push(Token::Star),
            '/' => out.push(Token::Slash),
            '(' => out.push(Token::LParen),
            ')' => out.push(Token::RParen),
            d if d.is_ascii_digit() => out.push(Token::Num(d as i64 - '0' as i64)),
            _ => {}
        }
    }
    out
}
'''
PARSER = '''//! A recursive-descent parser over `lexer::Token`.
use crate::lexer::Token;

pub enum Expr { Num(i64), Bin(Box<Expr>, Op, Box<Expr>) }
pub enum Op { Add, Sub, Mul, Div }

pub fn parse(tokens: &[Token]) -> Expr {
    let (e, _) = sum(tokens, 0);
    e
}

fn sum(t: &[Token], i: usize) -> (Expr, usize) {
    product(t, i)
}

fn product(t: &[Token], i: usize) -> (Expr, usize) {
    atom(t, i)
}

fn atom(t: &[Token], i: usize) -> (Expr, usize) {
    match t[i] { Token::Num(n) => (Expr::Num(n), i + 1), _ => panic!("expected a number") }
}
'''
EVAL = '''//! Evaluation.
use crate::parser::{Expr, Op};

pub fn eval(e: &Expr) -> i64 {
    match e {
        Expr::Num(n) => *n,
        Expr::Bin(a, op, b) => apply(eval(a), op, eval(b)),
    }
}

fn apply(a: i64, op: &Op, b: i64) -> i64 {
    match op { Op::Add => a + b, Op::Sub => a - b, Op::Mul => a * b, Op::Div => a / b }
}
'''
README = '''# octo-parser

A small arithmetic parser, written by many agents at once.

- `lexer.rs` turns text into tokens.
- `parser.rs` builds an expression tree.
- `eval.rs` computes it.
'''


def main():
    cmd(OWNER, cmd='create')
    # Restrict the repository: the viewer can read and nothing else.
    for user, role in [(REVIEWER, 'review'), (CHECKS, 'review'), (VIEWER, 'read')] + [(a, 'write') for a in AGENTS]:
        cmd(OWNER, cmd='grant', user=user, role=role)
    base = propose(OWNER, 'Start the parser: lexer, parser, evaluator and README', [],
                   {'src/lexer.rs': LEXER, 'src/parser.rs': PARSER, 'src/eval.rs': EVAL, 'README.md': README})
    review(base)
    cmd(OWNER, cmd='merge', channel='main', patch=base)
    snapshot = members()

    # Eight agents edit different lines of the same files from one snapshot.
    edits = [
        ('src/lexer.rs', 'Lex multi-digit numbers',
         "            d if d.is_ascii_digit() => out.push(Token::Num(d as i64 - '0' as i64)),\n",
         "            d if d.is_ascii_digit() => push_digit(&mut out, d),\n"),
        ('src/lexer.rs', 'Describe the token set in the module comment',
         '//! Tokens of a small arithmetic language.\n', '//! Tokens of a small arithmetic language: integers, + - * / and parentheses.\n'),
        ('src/parser.rs', 'Parse + and - with left associativity',
         '    product(t, i)\n', '    let (mut e, mut i) = product(t, i);\n    while let Some(op) = additive(t.get(i)) { let (r, j) = product(t, i + 1); e = Expr::Bin(Box::new(e), op, Box::new(r)); i = j; }\n    (e, i)\n'),
        ('src/parser.rs', 'Parse * and / with left associativity',
         '    atom(t, i)\n', '    let (mut e, mut i) = atom(t, i);\n    while let Some(op) = multiplicative(t.get(i)) { let (r, j) = atom(t, i + 1); e = Expr::Bin(Box::new(e), op, Box::new(r)); i = j; }\n    (e, i)\n'),
        ('src/parser.rs', 'Parenthesised expressions',
         '    match t[i] { Token::Num(n) => (Expr::Num(n), i + 1), _ => panic!("expected a number") }\n',
         '    match t[i] {\n        Token::Num(n) => (Expr::Num(n), i + 1),\n        Token::LParen => { let (e, j) = sum(t, i + 1); (e, j + 1) }\n        _ => panic!("expected a number or ("),\n    }\n'),
        ('src/eval.rs', 'Refuse division by zero instead of panicking',
         '    match op { Op::Add => a + b, Op::Sub => a - b, Op::Mul => a * b, Op::Div => a / b }\n',
         '    match op { Op::Add => a + b, Op::Sub => a - b, Op::Mul => a * b, Op::Div => a.checked_div(b).unwrap_or(0) }\n'),
        ('README.md', 'Document the grammar',
         '- `eval.rs` computes it.\n', '- `eval.rs` computes it.\n\n    sum     = product (("+" | "-") product)*\n    product = atom (("*" | "/") atom)*\n    atom    = number | "(" sum ")"\n'),
        ('src/eval.rs', 'Explain evaluation order in the module comment',
         '//! Evaluation.\n', '//! Evaluation, left operand first.\n'),
    ]
    texts = {'src/lexer.rs': LEXER, 'src/parser.rs': PARSER, 'src/eval.rs': EVAL, 'README.md': README}
    merged = []
    for agent, (path, intent, old, new) in zip(AGENTS, edits):
        assert old in texts[path], intent
        patch = propose(agent, intent, snapshot, {path: texts[path].replace(old, new, 1)})
        review(patch)
        merged.append(patch)
    cmd(OWNER, cmd='merge_batch', channel='main', patches=sorted(merged))

    # Two agents change the same line differently: a conflict to resolve.
    snapshot = members()
    current = {p: bytes(document(p, snapshot)['bytes']).decode() for p in texts}
    line = "            '-' => out.push(Token::Minus),\n"
    a = propose(AGENTS[8], 'Unary minus as its own token',
                snapshot, {'src/lexer.rs': current['src/lexer.rs'].replace(line, "            '-' => out.push(Token::Neg),\n")})
    b = propose(AGENTS[9], 'Lex "--" as a decrement operator',
                snapshot, {'src/lexer.rs': current['src/lexer.rs'].replace(line, "            '-' => out.push(minus_or_decrement(&mut chars)),\n")})
    review(a); review(b)
    cmd(OWNER, cmd='merge_batch', channel='main', patches=sorted([a, b]))

    # Waiting: ready to admit, unreviewed, and a failed check.
    snapshot = members()
    current = {p: bytes(document(p, snapshot)['bytes']).decode() for p in ['src/eval.rs', 'README.md']}
    ready = propose(AGENTS[0], 'Add a usage example to the README', snapshot,
                    {'README.md': current['README.md'] + '\n    assert_eq!(eval(&parse(&lex("2*(3+4)"))), 14);\n'})
    review(ready)
    propose(AGENTS[2], 'Support the modulo operator', snapshot,
            {'src/eval.rs': current['src/eval.rs'].replace('Op::Div =>', 'Op::Rem => a % b, Op::Div =>')})
    failing = propose(AGENTS[4], 'Evaluate with f64 instead of i64', snapshot,
                      {'src/eval.rs': current['src/eval.rs'].replace('i64', 'f64')})
    review(failing, passed=False)

    # An experiment on a forked channel.
    cmd(OWNER, cmd='fork', source='main', channel='experiment')
    trial = propose(AGENTS[6], 'Try a Pratt parser for operator precedence', snapshot,
                    {'README.md': current['README.md'] + '\nExperimental: precedence climbing with binding powers.\n'})
    review(trial)
    cmd(OWNER, cmd='merge', channel='experiment', patch=trial)

    state = api(OWNER, f'/api/state?repo={REPO}')
    print(f"{REPO}: {len(state['repository']['patches'])} patches, "
          f"{len(state['view']['patches'])} in main, {len(state['view']['conflicts'])} unresolved path(s)")
    print(f'viewer token (org {ORG}, user {VIEWER}):')
    print(token(VIEWER))


if __name__ == '__main__':
    main()
