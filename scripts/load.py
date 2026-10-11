#!/usr/bin/env python3
"""Measure authenticated collaboration operations and check resulting snapshots."""
from __future__ import annotations

import argparse
import concurrent.futures
import contextlib
import hashlib
import hmac
import json
import os
import pathlib
import platform
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request


class Failure(Exception):
    pass


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        # Never forward the Authorization header to a redirect destination.
        return None


class Driver:
    def __init__(self, origin, tokens, timeout):
        parsed = urllib.parse.urlsplit(origin)
        if (parsed.scheme not in ('http', 'https') or not parsed.netloc or parsed.username
                or parsed.password or parsed.path not in ('', '/') or parsed.query or parsed.fragment):
            raise Failure('URL must be an HTTP(S) origin without credentials, path, query or fragment')
        self.origin, self.tokens, self.timeout = origin.rstrip('/'), tokens, timeout
        self.lock = threading.Lock()
        self.measurements = {}
        self.started = time.perf_counter()

    def request(self, user, route, body=None, label='read'):
        payload = None if body is None else json.dumps(body, separators=(',', ':')).encode()
        req = urllib.request.Request(self.origin + route, data=payload, headers={
            'Authorization': 'Bearer ' + self.tokens[user], 'Content-Type': 'application/json'})
        started = time.perf_counter()
        status, size = 0, 0
        try:
            # Per-request openers have no shared mutable cookies or connections.
            with urllib.request.build_opener(NoRedirect).open(req, timeout=self.timeout) as response:
                status = response.status
                data = response.read()
                size = len(data)
                return json.loads(data)
        except urllib.error.HTTPError as error:
            status = error.code
            # Request headers and response bodies are excluded from failures.
            raise Failure(f'{label}: HTTP {status}') from None
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError):
            raise Failure(f'{label}: transport or response failure') from None
        finally:
            elapsed = time.perf_counter() - started
            with self.lock:
                self.measurements.setdefault(label, []).append((elapsed, status, size))

    def cmd(self, user, repo, operation, **fields):
        return self.request(user, '/api/command', dict(cmd=operation, repo=repo, **fields), operation)

    def view(self, repo, channel='main'):
        query = urllib.parse.urlencode(dict(repo=repo, channel=channel))
        return self.request('reviewer', '/api/state?' + query)['view']

    def ready(self, repo, patch):
        self.cmd('reviewer', repo, 'approve', patch=patch)
        self.cmd('checker', repo, 'check', patch=patch, passed=True)

    def refuse(self, user, repo, operation, **fields):
        label = 'expected_refusal_' + operation
        try:
            self.request(user, '/api/command', dict(cmd=operation, repo=repo, **fields), label)
        except Failure as error:
            if str(error) == f'{label}: HTTP 422':
                return
            raise
        raise Failure('admission policy accepted a prohibited operation')

    def stats(self):
        def percentile(values, p):
            values = sorted(values)
            return values[min(len(values) - 1, int((len(values) - 1) * p))] * 1000
        result = {}
        for label, samples in sorted(self.measurements.items()):
            durations = [s[0] for s in samples]
            result[label] = dict(requests=len(samples), failed=sum(s[1] != 200 for s in samples),
                                 p50_ms=percentile(durations, .50), p95_ms=percentile(durations, .95),
                                 p99_ms=percentile(durations, .99), max_ms=max(durations) * 1000,
                                 received_bytes=sum(s[2] for s in samples))
        return result


def issue(secret, tenant, user):
    payload = f'v1.{tenant}.{user}.{int(time.time()) + 86400}'
    return payload + '.' + hmac.new(secret.encode(), payload.encode(), hashlib.sha256).hexdigest()


def token_set(args, secret=None):
    if args.tokens:
        data = json.loads(pathlib.Path(args.tokens).read_text())
        tokens = {f'agent-{i}': token for i, token in enumerate(data['agents'])}
        tokens.update(reviewer=data['reviewer'], checker=data['checker'])
        if len(data['agents']) < args.agents:
            raise Failure('token file has fewer agent tokens than --agents')
        return tokens
    if secret is None:
        raise Failure('remote runs require --tokens; local test secrets are never read from production')
    return {**{f'agent-{i}': issue(secret, 1, i + 1) for i in range(args.agents)},
            'reviewer': issue(secret, 1, args.agents + 1),
            'checker': issue(secret, 1, args.agents + 2)}


@contextlib.contextmanager
def service(args):
    if not args.local_server:
        if not args.url:
            raise Failure('specify --url and --tokens, or --local-server')
        yield args.url, token_set(args), None, None
        return
    if args.tokens or args.url:
        raise Failure('--local-server cannot be combined with --url or --tokens')
    exe = pathlib.Path(args.local_server).resolve()
    if not exe.is_file():
        raise Failure('server binary not found; build o5s-server first')
    binary_sha256 = hashlib.sha256(exe.read_bytes()).hexdigest()
    temporary = tempfile.TemporaryDirectory(prefix='o5s-load-') if not args.data else None
    data = pathlib.Path(args.data if args.data else temporary.name)
    data.mkdir(parents=True, exist_ok=True)
    # Fresh run secret is retained only in memory and the child environment.
    secret = secrets.token_hex(32)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'
    env = dict(os.environ, H5I_APP_SECRET=secret, O5S_DATA=str(data), O5S_BIND=f'127.0.0.1:{port}')
    def start():
        return subprocess.Popen([str(exe)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    child = start()
    samples = {}
    monitor_done = threading.Event()
    def sample():
        while not monitor_done.wait(.1):
            pid = child.pid
            try:
                status = pathlib.Path(f'/proc/{pid}/status').read_text()
                rss = next(int(line.split()[1]) for line in status.splitlines() if line.startswith('VmRSS:'))
                stat = pathlib.Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
                ticks = int(stat[11]) + int(stat[12])
                item = samples.setdefault(pid, {'peak_rss_kib': 0, 'cpu_ticks': 0})
                item['peak_rss_kib'] = max(item['peak_rss_kib'], rss)
                item['cpu_ticks'] = max(item['cpu_ticks'], ticks)
            except (OSError, ValueError, StopIteration, IndexError):
                pass  # /proc is Linux-specific; report unavailable elsewhere.
    monitor = threading.Thread(target=sample, daemon=True)
    monitor.start()
    def metrics():
        entries = list(samples.values())
        if not entries:
            return {'sampling': 'unavailable'}
        return dict(sampling='Linux /proc, 100 ms; includes setup and restart',
                    server_binary_sha256=binary_sha256,
                    peak_server_rss_kib=max(item['peak_rss_kib'] for item in entries),
                    sampled_server_cpu_seconds=sum(item['cpu_ticks'] for item in entries) / os.sysconf('SC_CLK_TCK'),
                    retained_state_json_bytes=sum(path.stat().st_size for path in data.glob('*.json')))
    def wait():
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if child.poll() is not None:
                raise Failure('isolated server exited during startup')
            try:
                with urllib.request.urlopen(origin + '/health', timeout=.25):
                    return
            except urllib.error.HTTPError:
                return  # A 404 still establishes that the listener is ready.
            except (urllib.error.URLError, TimeoutError):
                time.sleep(.05)
        raise Failure('isolated server did not become ready')
    def restart():
        nonlocal child
        child.terminate()
        child.wait(timeout=10)
        child = start()
        wait()
    try:
        wait()
        yield origin, token_set(args, secret), restart, metrics
    finally:
        monitor_done.set()
        monitor.join(timeout=1)
        child.terminate()
        try:
            child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
        if temporary:
            temporary.cleanup()


def parallel(items, concurrency, task):
    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as executor:
        # executor.map keeps results in input order; request arrival order varies.
        return list(executor.map(task, items))


def setup(driver, repo, text, history):
    driver.cmd('agent-0', repo, 'create')
    base = None
    for generation in range(history + 1):
        base = driver.cmd('agent-0', repo, 'propose', intent=f'Load fixture generation {generation}',
                          dependencies=[] if base is None else [base],
                          edits=[dict(path='shared.txt', content=text)])['patch']
        driver.ready(repo, base)
        driver.cmd('reviewer', repo, 'merge', channel='main', patch=base)
    driver.cmd('reviewer', repo, 'fork', source='main', channel='reverse')
    return base


def run(args, driver, restart):
    prefix = 'load-' + str(time.time_ns())
    original = ''.join(f'base-{i}\n' for i in range(args.agents))
    repositories = [f'{prefix}-{i}' for i in range(args.agents)] if args.workload == 'repositories' else [prefix]
    bases = {repo: setup(driver, repo, original, args.history) for repo in repositories}
    # Exclude setup/readiness costs from workload throughput and latency.
    driver.measurements.clear()
    started = time.perf_counter()
    def propose(i):
        repo = repositories[i] if args.workload == 'repositories' else repositories[0]
        content = f'agent-{i}:' + ('x' * args.edit_bytes) + '\n'
        if args.workload in ('files', 'repositories'):
            patch = driver.cmd(f'agent-{i}', repo, 'propose', intent='Concurrent file fixture',
                               dependencies=[bases[repo]], edits=[dict(path=f'agent-{i}.txt', content=content)])['patch']
        else:
            line = 0 if args.workload == 'overlap' else i
            patch = driver.cmd(f'agent-{i}', repo, 'propose_lines', intent='Concurrent line fixture', dependencies=[],
                               edits=[dict(path='shared.txt', base=bases[repo], start=line, end=line + 1,
                                           content=content)])['patch']
        return repo, patch, content
    proposals = parallel(range(args.agents), args.concurrency, propose)
    proposal_seconds = time.perf_counter() - started
    first_repo, first_patch, _ = proposals[0]
    prior = driver.view(first_repo)
    driver.refuse('agent-0', first_repo, 'approve', patch=first_patch)
    driver.refuse('reviewer', first_repo, 'merge_batch', channel='main', patches=[first_patch])
    if driver.view(first_repo) != prior:
        raise Failure('a refused command changed channel state')
    parallel(proposals, args.concurrency, lambda record: driver.ready(record[0], record[1]))
    grouped = {repo: [] for repo in repositories}
    for repo, patch, _ in proposals:
        grouped[repo].append(patch)
    for repo, patches in grouped.items():
        for channel, delivery in [('main', patches), ('reverse', list(reversed(patches)))]:
            for index in range(0, len(delivery), args.batch):
                driver.cmd('reviewer', repo, 'merge_batch', channel=channel,
                           patches=delivery[index:index + args.batch])
    admission_seconds = time.perf_counter() - started
    snapshots = {}
    for repo in repositories:
        main, reverse = driver.view(repo), driver.view(repo, 'reverse')
        if main != reverse:
            raise Failure('delivery-order convergence check failed')
        snapshots[repo] = main
        acknowledged = set(grouped[repo])
        if not acknowledged.issubset(set(main['patches'])):
            raise Failure('an acknowledged admitted patch is missing')
        if args.workload == 'overlap' and args.agents > 1:
            alternatives = main['conflicts'].get('shared.txt', [])
            if len(alternatives) != args.agents:
                raise Failure('conflict alternatives were lost')
        else:
            if main['conflicts']:
                raise Failure('independent edits produced a conflict')
            if args.workload == 'disjoint':
                if main['files']['shared.txt'] != ''.join(record[2] for record in proposals):
                    raise Failure('disjoint edits did not materialize exactly')
            else:
                for i, (record_repo, _, content) in enumerate(proposals):
                    if record_repo == repo and main['files'].get(f'agent-{i}.txt') != content:
                        raise Failure('independent file content was lost')
    restart_seconds = None
    if args.restart:
        if restart is None:
            raise Failure('--restart requires an isolated --local-server')
        before = time.perf_counter()
        restart()
        for repo, expected in snapshots.items():
            if driver.view(repo) != expected:
                raise Failure('acknowledged snapshot changed after graceful restart')
        restart_seconds = time.perf_counter() - before
    return dict(run_id=prefix, workload=args.workload, agents=args.agents, concurrency=args.concurrency,
                historical_patches_per_repo=args.history + 1, edit_payload_bytes=args.edit_bytes,
                admission_batch_size=args.batch, proposal_seconds=proposal_seconds,
                proposals_per_second=args.agents / proposal_seconds, admission_seconds=admission_seconds,
                admitted_proposals_per_second=args.agents / admission_seconds,
                restart_seconds=restart_seconds, requests=driver.stats(),
                checks={'arrival_order_convergence': True, 'acknowledged_membership': True,
                        'materialized_bytes_or_conflict_alternatives': True,
                        'self_approval_and_unreviewed_admission_rejected': True,
                        'graceful_restart': bool(args.restart)},
                machine={'platform': platform.platform(), 'python': platform.python_version(),
                         'cpu_count': os.cpu_count()},
                limitations=['Closed-loop client concurrency, not fixed offered load.',
                             'Review/check results are API reports, not executed CI.',
                             'Graceful restart is not crash-injection or multi-process durability.',
                             'Current JSON store and one-process mutex are the measured backend.'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url')
    parser.add_argument('--tokens', help='JSON file with agents[], reviewer, checker bearer tokens')
    parser.add_argument('--local-server', metavar='BINARY', help='launch isolated server with fresh test credentials')
    parser.add_argument('--data', help='retain isolated server data at this path')
    parser.add_argument('--agents', type=int, default=32)
    parser.add_argument('--concurrency', type=int, default=16)
    parser.add_argument('--batch', type=int, default=100)
    parser.add_argument('--history', type=int, default=0, help='extra sequential fixture patches per repository')
    parser.add_argument('--edit-bytes', type=int, default=1024)
    parser.add_argument('--workload', choices=['disjoint', 'overlap', 'files', 'repositories'], default='disjoint')
    parser.add_argument('--timeout', type=float, default=60)
    parser.add_argument('--restart', action='store_true')
    parser.add_argument('--output', help='write report JSON to a file as well as stdout')
    args = parser.parse_args()
    if (min(args.agents, args.concurrency, args.batch) < 1 or args.history < 0
            or args.edit_bytes < 0 or args.timeout <= 0):
        parser.error('agents, concurrency and batch must be positive; history and edit-bytes nonnegative')
    driver = None
    try:
        with service(args) as (origin, tokens, restart, metrics):
            driver = Driver(origin, tokens, args.timeout)
            # Verify separate identities in the same tenant before generating fixtures.
            identities = []
            for key in [*(f'agent-{i}' for i in range(args.agents)), 'reviewer', 'checker']:
                role = 'author' if key.startswith('agent-') else key
                actor = driver.request(key, '/api/repositories', label='identity_probe_' + role)['actor']
                identities.append((actor['tenant'], actor['user']))
            if len(set(identities)) != len(identities) or len({x[0] for x in identities}) != 1:
                raise Failure('credentials must name distinct users in one tenant')
            identity_probe_requests = driver.stats()
            report = run(args, driver, restart)
            report['identity_probe_requests'] = identity_probe_requests
            if metrics is not None:
                report['local_backend_sample'] = metrics()
            output = json.dumps(report, indent=2, sort_keys=True) + '\n'
            if args.output:
                pathlib.Path(args.output).write_text(output)
            sys.stdout.write(output)
    except (Failure, OSError, KeyError, ValueError) as error:
        # No credential values, command arguments or request bodies in error output.
        reason = str(error) if isinstance(error, Failure) else type(error).__name__
        print('load run failed: ' + reason, file=sys.stderr)
        failed = dict(completed=False, failure=reason, workload=args.workload,
                      agents=args.agents, concurrency=args.concurrency,
                      historical_patches_per_repo=args.history + 1,
                      edit_payload_bytes=args.edit_bytes, admission_batch_size=args.batch,
                      timeout_seconds=args.timeout,
                      requests={} if driver is None else driver.stats())
        output = json.dumps(failed, indent=2, sort_keys=True) + '\n'
        if args.output:
            try:
                pathlib.Path(args.output).write_text(output)
            except OSError:
                print('failed report could not be written', file=sys.stderr)
        sys.stdout.write(output)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
