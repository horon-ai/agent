"""Contracts for independent CI gates and fallible compiler discovery."""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class WorkflowGuardTests(unittest.TestCase):
    def test_ci_builds_and_tests_cannot_silently_refresh_the_lockfile(self):
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        command = re.compile(r'(?:cargo|"\$CARGO_BIN"|"\$\(rustup which cargo\)") '
                             r'(?:build|check|test|clippy)\b')
        checked = 0
        for line in workflow.splitlines():
            if line.lstrip().startswith('#') or not command.search(line):
                continue
            with self.subTest(command=line.strip()):
                self.assertIn('--locked', line)
                checked += 1
        # Lowered from 15 because the browser liveness cohort, which held two of
        # these cargo test steps, no longer exists, again from 14 when the
        # provider matrix stage went with the vendor provider logins, and again
        # to 8 when the Build & Test cohort and e2e-compile `cargo test` steps
        # became one compile call plus `scripts/ci/run_lib_tests.py` (#19).
        # `run_lib_tests.py` executes the already built test executables and
        # never invokes cargo, so no unlocked build can hide behind the lower
        # floor; the assertion below pins that.
        self.assertGreaterEqual(checked, 8)
        self.assertIn('git diff --exit-code -- Cargo.lock', workflow)
        helper = (ROOT / 'scripts/ci/run_lib_tests.py').read_text()
        for invocation in ('cargo build', 'cargo test', 'cargo check', 'cargo clippy'):
            with self.subTest(invocation=invocation):
                self.assertNotIn(invocation, helper)

    def test_arm64_release_graph_is_a_mandatory_native_windows_check(self):
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        arm64 = workflow.split('  windows-arm64-check:\n', 1)[1].split(
            '\n  windows-cross-check:', 1)[0]
        self.assertIn('runs-on: windows-latest', arm64)
        self.assertIn('arch: amd64_arm64', arm64)
        self.assertIn('targets: aarch64-pc-windows-msvc', arm64)
        self.assertIn('JUSTRUST_TOOLCHAIN: system', arm64)
        self.assertIn('JUSTRUST_DISABLE: "1"', arm64)
        self.assertIn('& justrust check --local --locked -p horon-agent --bins '
                      '--target aarch64-pc-windows-msvc '
                      '--no-default-features --features pdf', arm64)
        self.assertIn('if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }', arm64)
        self.assertNotIn('continue-on-error:', arm64)
        # The only allowed condition is the negation of the repository-wide
        # switch to the self-hosted macOS runner, which cannot run Windows jobs
        # at all. The negation keeps fork pull requests on hosted runners.
        conditions = [line.strip() for line in arm64.splitlines() if line.strip().startswith('if:')]
        self.assertEqual(conditions, [
            "if: ${{ !(vars.CI_SELF_HOSTED == 'true' && (github.event_name != 'pull_request' "
            "|| github.event.pull_request.head.repo.full_name == github.repository)) }}",
        ])

    def test_self_hosted_runners_never_serve_fork_pull_requests(self):
        # Any selection of the self-hosted macOS runner must carry the
        # first-party-only guard, so a fork pull request always falls back to a
        # hosted runner instead of executing untrusted code on the repository's
        # own machine.
        guard = ('github.event.pull_request.head.repo.full_name '
                 '== github.repository')
        selections = 0
        for path in sorted((ROOT / '.github/workflows').glob('*.yml')):
            for number, line in enumerate(path.read_text().splitlines(), 1):
                if 'self-hosted' not in line or line.lstrip().startswith('#'):
                    continue
                with self.subTest(workflow=path.name, line=number):
                    self.assertIn(guard, line)
                if 'runs-on:' in line:
                    selections += 1
        self.assertGreaterEqual(selections, 5)

    def test_quality_gates_report_failures_without_weakening_them(self):
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        quality = workflow.split('  quality:\n', 1)[1].split('\n  release-automation:', 1)[0]
        steps = re.split(r'^      - name: ', quality, flags=re.MULTILINE)[1:]
        self.assertGreaterEqual(len(steps), 15)
        for step in steps:
            name = step.splitlines()[0]
            with self.subTest(step=name):
                # The initial module-resolution check runs normally. Every
                # independent check after it must run after earlier failures,
                # stop on cancellation, and retain its failing exit status.
                if name != 'Check module declarations resolve':
                    # Runs after an earlier failure and stops on cancellation.
                    # A step may narrow further (e.g. one OS), never widen.
                    self.assertRegex(step, r'if: \$\{\{ !cancelled\(\)')
                self.assertNotRegex(step, r'\bcontinue-on-error:\s*true\b')
        self.assertNotIn('continue-on-error: true', quality)
        self.assertIn('cargo clippy --locked --all-targets --all-features -- -D warnings', quality)
        for script in ('check_code_size.py', 'check_test_size.py',
                       'check_panics.py', 'check_swallowed_errors.py'):
            self.assertIn(f'run: python3 scripts/ci/{script}\n', quality)

    def test_compiler_lookup_failure_is_not_masked_by_export(self):
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        match = re.search(r'^          (RUSTC=.*)\n          (export RUSTC)\n'
                          r'          (CARGO_BIN=.*)\n', workflow, re.MULTILINE)
        self.assertIsNotNone(match, 'compiler discovery must be separate from export')
        fragment = '\n'.join(match.groups()) + '\nprintf should-not-run\n'
        with tempfile.TemporaryDirectory() as temp:
            rustup = Path(temp) / 'rustup'
            rustup.write_text('#!/bin/sh\nexit 17\n')
            rustup.chmod(0o755)
            env = {key: value for key, value in os.environ.items()
                   if not key.startswith('BASH_FUNC_') and key != 'BASH_ENV'}
            env['PATH'] = f'{temp}:{os.environ["PATH"]}'
            result = subprocess.run(['/bin/bash', '-eu', '-c', fragment],
                                    env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 17, result.stderr)
            self.assertNotIn('should-not-run', result.stdout)

    def test_branch_filters_name_only_branches_that_exist(self):
        # The repository has `main` and `dev`; `master` never existed publicly.
        # A filter on a missing branch silently never fires.
        for name in ('ci.yml', 'structural-perf.yml'):
            with self.subTest(workflow=name):
                text = (ROOT / f'.github/workflows/{name}').read_text()
                self.assertIn('branches: [dev, main]', text)
                self.assertNotIn('master', text)
                self.assertIn('pull_request:', text)
                self.assertIn('push:', text)

    def test_ci_dispatch_and_cancellation_keep_release_evidence(self):
        # `scripts/release/check_ci.py` accepts only a successful push run for the
        # tagged SHA, so a push run must never be superseded by the next one, and
        # a lost run must be recreatable for the same commit.
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        self.assertIn('workflow_dispatch:', workflow)
        self.assertIn("cancel-in-progress: ${{ github.event_name == 'pull_request' }}",
                      workflow)
        self.assertNotIn('cancel-in-progress: true', workflow)

    def test_every_job_has_a_hosted_sized_timeout(self):
        for name in ('ci.yml', 'structural-perf.yml'):
            text = (ROOT / f'.github/workflows/{name}').read_text()
            jobs = text.split('\njobs:\n', 1)[1]
            starts = list(re.finditer(r'^  ([A-Za-z0-9_-]+):\n', jobs, re.MULTILINE))
            self.assertGreaterEqual(len(starts), 1 if name == 'structural-perf.yml' else 11)
            for index, start in enumerate(starts):
                end = starts[index + 1].start() if index + 1 < len(starts) else len(jobs)
                with self.subTest(workflow=name, job=start.group(1)):
                    self.assertIn('timeout-minutes:', jobs[start.start():end])

    def test_linux_rust_jobs_free_disk_before_building(self):
        # A hosted ubuntu runner has ~14 GB on `/`; the workspace's cold builds do
        # not fit while the pre-installed toolchains are there.
        text = (ROOT / '.github/workflows/structural-perf.yml').read_text()
        self.assertIn('sudo rm -rf /usr/share/dotnet /usr/local/lib/android /opt/ghc',
                      text)
        self.assertIn('/opt/hostedtoolcache/CodeQL', text)
        self.assertIn('df -h /', text)

        # ci.yml delegates to one script so the four steps cannot drift apart
        # (#21). Every Linux cleanup step must call it and stay off a
        # persistent self-hosted machine.
        script = (ROOT / 'scripts/ci/free_disk_space.sh').read_text()
        for path in ('/usr/share/dotnet', '/usr/local/lib/android', '/opt/ghc',
                     '/opt/hostedtoolcache/CodeQL', '/usr/local/share/boost'):
            with self.subTest(path=path):
                self.assertIn(path, script)
        self.assertIn('df -h /', script)
        self.assertIn('--wait', script)

        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        steps = re.split(r'^      - name: ', workflow, flags=re.MULTILINE)
        cleanup = [step for step in steps if step.startswith('Free disk space')]
        linux = [step for step in cleanup if '(macOS)' not in step.splitlines()[0]]
        self.assertEqual(len(linux), 3, 'quality, build, and windows-cross-check')
        for step in linux:
            with self.subTest(step=step.splitlines()[0]):
                self.assertIn('run: bash scripts/ci/free_disk_space.sh', step)
                self.assertIn("runner.environment == 'github-hosted'", step)
        # The macOS step is out of scope and keeps its own body.
        self.assertIn('sudo rm -rf /Users/runner', workflow)
        marker = workflow.index('sudo rm -rf /Users/runner')
        step_start = workflow.rindex('      - name: ', 0, marker)
        self.assertIn("runner.environment == 'github-hosted'", workflow[step_start:marker])

    def test_workflows_use_maintained_action_majors(self):
        for name in ('ci.yml', 'structural-perf.yml'):
            text = (ROOT / f'.github/workflows/{name}').read_text()
            with self.subTest(workflow=name):
                self.assertNotIn('actions/checkout@v4', text)
                self.assertIn('actions/checkout@v7', text)
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        for action in ('actions/upload-artifact@v7',
                       'actions/setup-node@v7',
                       'astral-sh/setup-uv@v10.3.3',
                       'mozilla-actions/sccache-action@v0.0.11'):
            with self.subTest(action=action):
                self.assertIn(action, workflow)

    def test_cross_target_jobs_pin_the_toolchain_they_installed_targets_into(self):
        # `dtolnay/rust-toolchain@stable` installs into the `stable` toolchain by
        # name. Commands run inside the checkout resolve rust-toolchain.toml to a
        # different toolchain directory, so without a pin the installed Windows
        # targets are invisible to justrust and cargo-xwin.
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        for job, following in (('windows-arm64-check', 'windows-cross-check'),
                               ('windows-cross-check', None)):
            block = workflow.split(f'  {job}:\n', 1)[1]
            if following:
                block = block.split(f'\n  {following}:', 1)[0]
            with self.subTest(job=job):
                self.assertIn('RUSTUP_TOOLCHAIN: stable', block)
                self.assertIn('targets: ', block)


if __name__ == '__main__':
    unittest.main()
