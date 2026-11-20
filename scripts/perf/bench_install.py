#!/usr/bin/env python3
"""Measure a public release install and its first successful version invocation."""

import argparse
import contextlib
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
COMPETITORS = ('jcode', 'claude_code', 'codex', 'opencode', 'pi', 'omp', 'omo')
WINDOWS = os.name == 'nt'
# The official installers write real user state, so the measurement needs a
# disposable machine, and five samples per state is the published floor.
MIN_RUNS = 5


def child_env(root, tag):
    # The comparison invokes third-party npm installers. Give each pair a fresh
    # user environment, including its own package cache and install directory.
    inherited = ('PATH', 'SystemRoot', 'SystemDrive', 'WINDIR', 'COMSPEC', 'PATHEXT',
                 'ProgramFiles', 'ProgramFiles(x86)', 'ProgramW6432',
                 'PROCESSOR_ARCHITECTURE', 'PROCESSOR_ARCHITEW6432', 'NUMBER_OF_PROCESSORS',
                 'LANG', 'LC_ALL', 'SSL_CERT_FILE', 'SSL_CERT_DIR')
    env = {k: os.environ[k] for k in inherited if k in os.environ}
    home, scratch, local = root / 'home', root / 'tmp', root / 'local'
    for path in (home, scratch, local):
        path.mkdir()
    env.update(HOME=str(home), USERPROFILE=str(home), LOCALAPPDATA=str(local),
               APPDATA=str(home / 'roaming'), TMPDIR=str(scratch), TEMP=str(scratch), TMP=str(scratch),
               XDG_CONFIG_HOME=str(home / '.config'), XDG_CACHE_HOME=str(home / '.cache'),
               HORON_HOME=str(home / '.horon'), HORON_AGENT_INSTALL_DIR=str(root / 'bin'),
               HORON_AGENT_VERSION=tag, HORON_AGENT_SKIP_SERVER_RELOAD='1',
               HORON_AGENT_NO_AUTO_UPDATE='1', npm_config_cache=str(root / 'npm-cache'),
               npm_config_update_notifier='false', CI='1', NO_COLOR='1')
    # The one jcode release this round resolved, so every platform installs and
    # later reports the same identity. JCODE_VERIFY_VERSION keeps the resolver
    # from executing the binary: the timed first `--version` below must be its
    # first run, and it checks the value from that same call.
    for key in ('JCODE_VERSION', 'JCODE_RELEASE_COMMIT', 'JCODE_RELEASE_SOURCE'):
        if key in os.environ:
            env[key] = os.environ[key]
    env['JCODE_VERIFY_VERSION'] = '0'
    env['JCODE_METADATA'] = str(root / 'jcode-release.json')
    if WINDOWS:
        # The runner starts Python from pwsh 7. powershell.exe uses its own
        # inbox modules (including the Get-FileHash cmdlet), even on ARM64.
        env['PSModulePath'] = str(Path(env['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/Modules')
    return env


def require_hosted_runner(parser, runs):
    # Checked before anything is installed, on every OS: the runners are the
    # only disposable machines, and the report is only comparable at the floor.
    if os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted':
        parser.error('install timing needs a disposable GitHub-hosted runner '
                     '(GITHUB_ACTIONS=true, RUNNER_ENVIRONMENT=github-hosted)')
    if runs < MIN_RUNS:
        parser.error(f'install timing needs at least {MIN_RUNS} runs per state, got {runs}')


@contextlib.contextmanager
def original_user_path():
    if not WINDOWS:
        yield
        return
    # The official PowerShell installer persists HKCU\Environment\Path. Use a
    # disposable runner and restore the precise value and registry type per pair.
    import winreg
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, 'Environment', 0, winreg.KEY_READ | winreg.KEY_SET_VALUE) as key:
        try:
            saved = winreg.QueryValueEx(key, 'Path')
        except FileNotFoundError:
            saved = None
        try:
            yield
        finally:
            if saved is None:
                with contextlib.suppress(FileNotFoundError):
                    winreg.DeleteValue(key, 'Path')
            else:
                winreg.SetValueEx(key, 'Path', 0, saved[1], saved[0])


def terminate_pair_processes(root):
    # The runner may expose TEMP through a short Windows path. Python resolves
    # it to the long form. Match either complete directory with a path boundary.
    cleanup = r'''
$prefixes = @($env:BENCH_PAIR, $env:BENCH_PAIR_RESOLVED) | ForEach-Object {
    [IO.Path]::GetFullPath($_).TrimEnd('\') + '\'
}
foreach ($p in [Diagnostics.Process]::GetProcesses()) {
    try {
        try { $path = $p.MainModule.FileName } catch { continue }
        if (-not $path) { continue }
        foreach ($prefix in $prefixes) {
            if ($path.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
                try {
                    $p.Kill()
                    if (-not $p.WaitForExit(5000)) { throw 'Timed out stopping a benchmark process' }
                } catch [InvalidOperationException] { } # The process has already exited.
                break
            }
        }
    } finally { $p.Dispose() }
}
if (-not $?) { exit 1 }
'''
    # Only what powershell.exe needs to start: no token or secret from the
    # runner environment reaches a child process for a housekeeping step.
    env = {k: os.environ[k] for k in ('SystemRoot', 'SystemDrive', 'windir', 'COMSPEC', 'PATH', 'PATHEXT')
           if k in os.environ}
    env['BENCH_PAIR'] = str(root)
    env['BENCH_PAIR_RESOLVED'] = str(root.resolve(strict=True))
    subprocess.run(['powershell.exe', '-NoLogo', '-NoProfile', '-NonInteractive', '-Command', cleanup],
                   env=env, stdin=subprocess.DEVNULL, timeout=120, check=True)


@contextlib.contextmanager
def pair_directory(problems):
    # Cleanup problems are recorded in `problems`, never raised here, so the
    # measured rows still reach the report; the caller turns them into a failure.
    # TemporaryDirectory keeps its read-only-file handling on Windows.
    folder = tempfile.TemporaryDirectory(prefix='bench-install-')
    root = Path(folder.name)
    try:
        yield root
    finally:
        if WINDOWS:
            try:
                terminate_pair_processes(root)
            except (OSError, subprocess.SubprocessError) as exc:
                problems.append(f'process cleanup failed: {exc}')
                print(f'warning: process cleanup failed: {exc}', file=sys.stderr, flush=True)
        try:
            folder.cleanup()
        except (OSError, subprocess.SubprocessError) as exc:
            problems.append(f'installed files left behind: {exc}')
            print(f'warning: installed files left behind: {exc}', file=sys.stderr, flush=True)


def version_summary(values):
    if not values:
        return None
    mid = statistics.median(values)
    return {'n': len(values), 'median': round(mid, 3), 'min': min(values), 'max': max(values),
            'mad': round(statistics.median(abs(x - mid) for x in values), 3)}


def read_release(path: Path) -> dict | None:
    """The release record install_competitors.sh wrote, or None when absent."""
    try:
        data = json.loads(Path(path).read_text(encoding='utf-8'))
    except (OSError, ValueError):
        return None
    return data if isinstance(data, dict) else None


def run_sample(tool, checkout, root, env, tag):
    dest = root / 'installed'
    horon = tool in ('horon', 'horon-base')
    if horon and WINDOWS:
        cmd = ['powershell.exe', '-NoLogo', '-NoProfile', '-NonInteractive', '-File',
               str(checkout / 'scripts/install/install.ps1'), '-Version', tag, '-InstallDir', str(root / 'bin')]
    elif horon:
        cmd = ['bash', str(checkout / 'scripts/install/install.sh')]
    else:
        # Use the same pinned packages, launchers and release checksum checks as compare.
        bash = shutil.which('bash', path=env['PATH'])
        if WINDOWS:
            git = shutil.which('git', path=env['PATH'])
            if git:
                candidate = Path(git).resolve().parents[1] / 'bin/bash.exe'
                if candidate.is_file():
                    bash = str(candidate)
        cmd = [bash or 'bash', str(ROOT / 'scripts/perf/install_competitors.sh'), str(dest), tool]
    # Resolving and verifying a competitor asks GitHub's API once per sample.
    # Keep the job token on the installer process, never on the measured binary:
    # the timed `--version` below runs with `env`, which has no credential.
    install_env = env
    if not horon:
        tokens = {key: os.environ[key] for key in ('GH_TOKEN', 'GITHUB_TOKEN') if key in os.environ}
        if tokens:
            install_env = {**env, **tokens}
    start = time.perf_counter_ns()
    result = subprocess.run(cmd, cwd=root, env=install_env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, timeout=420)
    installed = time.perf_counter_ns()
    if result.returncode:
        raise ValueError(f'installer exited {result.returncode}: {result.stdout.decode(errors="replace")[-600:]}')
    if horon:
        binary = root / 'bin' / ('horon-agent.exe' if WINDOWS else 'horon-agent')
    else:
        prefix = f'--bin {tool}='
        matches = [s[len(prefix):] for s in (dest / 'bins.args').read_text().splitlines() if s.startswith(prefix)]
        if len(matches) != 1:
            raise ValueError((dest / 'failures.txt').read_text(errors='replace').strip() or 'missing installed CLI')
        binary = Path(matches[0])
        # install_competitors can bootstrap Bun into the throwaway prefix.
        bun_dir = dest / 'bun/node_modules/.bin'
        if bun_dir.is_dir():
            env = {**env, 'PATH': str(bun_dir) + os.pathsep + env['PATH']}
    launched = time.perf_counter_ns()
    version = subprocess.run([str(binary), '--version'], cwd=root, env=env, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=30)
    end = time.perf_counter_ns()
    text = version.stdout.decode(errors='replace').strip()
    if version.returncode or not text or (horon and not re.search(r'\bv?' + re.escape(tag[1:]) + r'(?![\w.\-])', text)):
        raise ValueError(f'first --version failed ({version.returncode}): {text[-300:]}')
    row = {'install_ms': round((installed - start) / 1e6, 3),
           'first_version_ms': round((end - launched) / 1e6, 3),
           'total_ms': round((end - start) / 1e6, 3), 'version': text[:200]}
    if not horon:
        # The release identity is read after the timed call so the file read never
        # lands inside install_ms or first_version_ms. For jcode this same first
        # call must name the resolved release: the installer was told not to run
        # the binary earlier, so nothing warmed it.
        release = read_release(Path(env.get('JCODE_METADATA') or ''))
        if tool == 'jcode':
            expected = (release or {}).get('version')
            if not expected:
                raise ValueError('installer did not record jcode-release.json')
            if not re.search(r'(?<![\w.])v?' + re.escape(expected) + r'(?![\w.\-])', text):
                raise ValueError(f'first --version reports {text[:160]!r}, expected {expected}')
        if release:
            row['release'] = {key: release.get(key) for key in
                              ('tag', 'version', 'commit', 'asset', 'sha256', 'download_url', 'source')}
    return row


def checkout_revision(path):
    # The raw A/B report names the exact commit each side ran, so a difference
    # between rounds is attributable without trusting the refs the caller passed.
    # A checkout without git records null instead of a guessed identity.
    try:
        result = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=path, stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=60)
    except (OSError, subprocess.SubprocessError):
        return None
    head = result.stdout.decode(errors='replace').strip()
    return head if result.returncode == 0 and re.fullmatch(r'[0-9a-f]{40,64}', head) else None


def summarise(tools, rows, runs):
    # A failed install is an attempt. A warm row skipped after a cold failure is not.
    summary = {}
    for name in tools:
        for state in ('cold', 'warm'):
            group = [r for r in rows if r['tool'] == name and r['state'] == state]
            valid = [r for r in group if 'total_ms' in r]
            skipped = sum(1 for r in group if r.get('skipped'))
            summary[f'{name}/{state}'] = {
                'planned': runs, 'attempted': len(group) - skipped, 'skipped': skipped,
                'failed': len(group) - skipped - len(valid),
                **{metric: version_summary([r[metric] for r in valid])
                   for metric in ('install_ms', 'first_version_ms', 'total_ms')}}
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--runs', type=int, default=MIN_RUNS)
    parser.add_argument('--competitors', default=','.join(COMPETITORS))
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--baseline-dir', type=Path)
    args = parser.parse_args()
    others = list(dict.fromkeys(x.strip() for x in args.competitors.split(',') if x.strip()))
    if not re.fullmatch(r'v\d+\.\d+\.\d+', args.tag) or set(others) - set(COMPETITORS):
        parser.error('expected a public vX.Y.Z tag and known competitors')
    require_hosted_runner(parser, args.runs)
    # The code under test is independent of the harness: CI checks the candidate
    # out at CANDIDATE_DIR, and only the install path of that tree is exercised.
    try:
        candidate_dir = Path(os.environ.get('CANDIDATE_DIR') or ROOT).resolve(strict=True)
        baseline_dir = args.baseline_dir.resolve(strict=True) if args.baseline_dir else None
    except OSError as exc:
        parser.error(f'checkout directory is missing: {exc}')
    tools = ['horon', *others]
    if baseline_dir:
        tools.insert(1, 'horon-base')
    rows, cleanup_errors = [], []
    report = {'suite': 'install', 'tag': args.tag, 'system': platform.system(), 'arch': platform.machine(),
              'release': platform.release(), 'runs': args.runs, 'samples': rows, 'summary_ms': {},
              'releases': {},
              'candidate_head': os.environ.get('INSTALL_CANDIDATE_HEAD') or checkout_revision(candidate_dir),
              'baseline_head': os.environ.get('INSTALL_BASELINE_HEAD') or (checkout_revision(baseline_dir) if baseline_dir else None),
              'cleanup_errors': cleanup_errors}
    try:
        for i in range(args.runs):
            # Reverse before/after each round, and rotate where each pair lands.
            order = tools if i % 2 == 0 else tools[::-1]
            offset = (i // 2) % len(order)
            for name in order[offset:] + order[:offset]:
                checkout = baseline_dir if name == 'horon-base' else candidate_dir
                with original_user_path(), pair_directory(cleanup_errors) as root:
                    env = child_env(root, args.tag)
                    cold_ok = True
                    for state in ('cold', 'warm'):
                        row = {'tool': name, 'round': i + 1, 'state': state}
                        if cold_ok:
                            try:
                                row.update(run_sample(name, checkout, root, env, args.tag))
                            except (OSError, ValueError, subprocess.TimeoutExpired) as exc:
                                row['error'] = str(exc)[:700]
                                cold_ok = False
                            if row.get('release') and name == 'jcode':
                                report['releases']['jcode'] = row['release']
                        else:
                            row['error'] = 'cold install failed; no warm destination'
                            row['skipped'] = True
                        rows.append(row)
                        print(name, i + 1, state, row.get('total_ms', row.get('error')), flush=True)
    finally:
        # Publish what was measured even when a round aborts, so an unfinished
        # run leaves a report that names the shortfall instead of nothing.
        report['summary_ms'] = summarise(tools, rows, args.runs)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2, ensure_ascii=False) + '\n', encoding='utf-8')
    # jcode is a resolved, verified release rather than a best-effort competitor:
    # a failed resolution, checksum or version check fails the job after the
    # report is written. Other competitors only degrade the table.
    failed = (bool(cleanup_errors)
              or any('error' in r for r in rows if r['tool'] in ('horon', 'horon-base'))
              or any('error' in r for r in rows if r['tool'] == 'jcode'))
    return int(failed)


if __name__ == '__main__':
    sys.exit(main())
