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
    if WINDOWS:
        # The runner starts Python from pwsh 7. powershell.exe uses its own
        # inbox modules (including the Get-FileHash cmdlet), even on ARM64.
        env['PSModulePath'] = str(Path(env['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/Modules')
    return env


@contextlib.contextmanager
def original_user_path():
    if not WINDOWS:
        yield
        return
    # The official PowerShell installer persists HKCU\Environment\Path. Use a
    # disposable runner and restore the precise value and registry type per pair.
    if os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted':
        raise ValueError('Windows install timing requires a disposable GitHub-hosted runner')
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


@contextlib.contextmanager
def pair_directory():
    with tempfile.TemporaryDirectory(prefix='bench-install-') as folder:
        root = Path(folder)
        try:
            yield root
        finally:
            if WINDOWS:
                # A CLI may leave a helper running from its install directory.
                # Only handles to binaries installed in this pair belong to us.
                cleanup = r'''
$prefix = [IO.Path]::GetFullPath($env:BENCH_PAIR) + [IO.Path]::DirectorySeparatorChar
foreach ($p in [Diagnostics.Process]::GetProcesses()) {
    try {
        if ($p.MainModule.FileName.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
            $p.Kill()
            [void]$p.WaitForExit(5000)
        }
    } catch [ComponentModel.Win32Exception] {} catch [InvalidOperationException] {}
    finally { $p.Dispose() }
}'''
                subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', cleanup],
                               env={**os.environ, 'BENCH_PAIR': str(root)}, check=True, timeout=30)


def version_summary(values):
    if not values:
        return None
    mid = statistics.median(values)
    return {'n': len(values), 'median': round(mid, 3), 'min': min(values), 'max': max(values),
            'mad': round(statistics.median(abs(x - mid) for x in values), 3)}


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
    start = time.perf_counter_ns()
    result = subprocess.run(cmd, cwd=root, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
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
    return {'install_ms': round((installed - start) / 1e6, 3),
            'first_version_ms': round((end - launched) / 1e6, 3),
            'total_ms': round((end - start) / 1e6, 3), 'version': text[:200]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--runs', type=int, default=5)
    parser.add_argument('--competitors', default=','.join(COMPETITORS))
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--baseline-dir', type=Path)
    args = parser.parse_args()
    others = list(dict.fromkeys(x.strip() for x in args.competitors.split(',') if x.strip()))
    if not re.fullmatch(r'v\d+\.\d+\.\d+', args.tag) or args.runs < 1 or set(others) - set(COMPETITORS):
        parser.error('expected a public vX.Y.Z tag, positive runs and known competitors')
    tools = ['horon', *others]
    if args.baseline_dir:
        tools.insert(1, 'horon-base')
    rows = []
    for i in range(args.runs):
        # Reverse before/after each round, and rotate where each pair lands.
        order = tools if i % 2 == 0 else tools[::-1]
        offset = (i // 2) % len(order)
        for name in order[offset:] + order[:offset]:
            checkout = args.baseline_dir.resolve() if name == 'horon-base' else ROOT
            with original_user_path(), pair_directory() as root:
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
                    else:
                        row['error'] = 'cold install failed; no warm destination'
                    rows.append(row)
                    print(name, i + 1, state, row.get('total_ms', row.get('error')), flush=True)
    summary = {}
    for name in tools:
        for state in ('cold', 'warm'):
            valid = [r for r in rows if r['tool'] == name and r['state'] == state and 'total_ms' in r]
            summary[f'{name}/{state}'] = {
                metric: version_summary([r[metric] for r in valid])
                for metric in ('install_ms', 'first_version_ms', 'total_ms')}
    report = {'suite': 'install', 'tag': args.tag, 'system': platform.system(), 'arch': platform.machine(),
              'release': platform.release(), 'runs': args.runs, 'samples': rows, 'summary_ms': summary}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, ensure_ascii=False) + '\n', encoding='utf-8')
    return int(any('error' in r for r in rows if r['tool'] in ('horon', 'horon-base')))


if __name__ == '__main__':
    sys.exit(main())
