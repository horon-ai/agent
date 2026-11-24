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
        conditions = [line.strip() for line in arm64.splitlines() if line.strip().startswith('if:')]
        self.assertEqual(conditions, ["if: ${{ needs.changes.outputs.rust == 'true' }}"])

    def test_workflows_do_not_select_self_hosted_runners(self):
        for path in (ROOT / '.github/workflows').iterdir():
            if path.suffix not in {'.yml', '.yaml'}:
                continue
            with self.subTest(workflow=path.name):
                for line in path.read_text().splitlines():
                    config = line.partition('#')[0].lower()
                    self.assertNotIn('self-hosted', config)
                    self.assertNotIn('ci_self_hosted', config)

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
                       'check_panics.py', 'check_swallowed_errors.py',
                       'check_growth_ratchets.py'):
            self.assertIn(f'run: python3 scripts/ci/{script}\n', quality)
        # The growth ratchets discover their own ceiling files, so the contract
        # tests that pin that discovery are a step of their own.
        self.assertIn('run: python3 -m unittest -v scripts/ci/test_check_growth_ratchets.py\n',
                      quality)

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
        # tagged SHA, so a push run on `main` must never be superseded by the next
        # one, and a lost run must be recreatable for the same commit. Everywhere
        # else a newer run replaces the older one; a manual dispatch is diagnosis
        # only and never counts as release evidence.
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        self.assertIn('workflow_dispatch:', workflow)
        # One group per ref: a shared group would let a `dev` run cancel `main`.
        self.assertIn('group: ci-${{ github.workflow }}-${{ github.ref }}', workflow)
        self.assertIn("cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}",
                      workflow)
        self.assertNotIn('cancel-in-progress: true', workflow)
        self.assertNotIn("cancel-in-progress: ${{ github.event_name == 'pull_request' }}",
                         workflow)

    def test_every_job_has_a_hosted_sized_timeout(self):
        # `soak.yml` runs the 1,000-cycle form, whose own bound is the long one,
        # but a hung run must still give its runner back.
        for name in ('ci.yml', 'structural-perf.yml', 'soak.yml'):
            text = (ROOT / f'.github/workflows/{name}').read_text()
            jobs = text.split('\njobs:\n', 1)[1]
            starts = list(re.finditer(r'^  ([A-Za-z0-9_-]+):\n', jobs, re.MULTILINE))
            minimum = {'ci.yml': 11, 'structural-perf.yml': 1, 'soak.yml': 1}[name]
            self.assertGreaterEqual(len(starts), minimum)
            for index, start in enumerate(starts):
                end = starts[index + 1].start() if index + 1 < len(starts) else len(jobs)
                with self.subTest(workflow=name, job=start.group(1)):
                    self.assertIn('timeout-minutes:', jobs[start.start():end])

    def test_linux_rust_jobs_free_disk_before_building(self):
        # A hosted ubuntu runner has ~14 GB on `/`; the workspace's cold builds do
        # not fit while the pre-installed toolchains are there. structural-perf
        # calls the same shared script as ci.yml, after the checkout that puts it
        # on disk, and never deletes inline again (#21).
        text = (ROOT / '.github/workflows/structural-perf.yml').read_text()
        gate = text.split('\n  structural-perf:\n', 1)[1].split(
            '\n  structural-perf-required:\n', 1)[0]
        self.assertIn('run: bash scripts/ci/free_disk_space.sh', gate)
        # The condition is matched as the step's own `if:` line, so the same text
        # in a comment cannot stand in for it on the self-hosted macOS runner.
        self.assertRegex(
            gate,
            re.compile(r"^        if: \$\{\{ runner\.environment == 'github-hosted'"
                       r" && runner\.os == 'Linux' \}\}$", re.MULTILINE))
        self.assertNotIn('sudo rm -rf /usr/share/dotnet', gate)
        self.assertLess(gate.index('- uses: actions/checkout@'),
                        gate.index('- name: Free disk space'),
                        'the shared script must be checked out before it runs')

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
        self.assertEqual(len(linux), 4, 'quality, build, windows-cross-check, and docs-corpus')
        for step in linux:
            with self.subTest(step=step.splitlines()[0]):
                self.assertIn('run: bash scripts/ci/free_disk_space.sh', step)
                self.assertIn("runner.environment == 'github-hosted'", step)
        # The macOS step is out of scope and keeps its own body.
        self.assertIn('sudo rm -rf /Users/runner', workflow)
        marker = workflow.index('sudo rm -rf /Users/runner')
        step_start = workflow.rindex('      - name: ', 0, marker)
        self.assertIn("runner.environment == 'github-hosted'", workflow[step_start:marker])

    def test_workflows_pin_actions_to_commits_of_maintained_majors(self):
        pinned = ('ci.yml', 'structural-perf.yml', 'require-issue.yml', 'freebsd-smoke.yml',
                  'windows-smoke.yml', 'loading-ab.yml', 'benchmark.yml',
                  'benchmark-os.yml', 'benchmark-docs.yml', 'release-recovery.yml',
                  'soak.yml', 'package-compat.yml')
        use = re.compile(r'^\s*(?:- )?uses: (\S+)@(\S+)(?: # (\S+))?\s*$')
        for name in pinned:
            text = (ROOT / f'.github/workflows/{name}').read_text()
            for number, line in enumerate(text.splitlines(), 1):
                match = use.match(line)
                if not match or match.group(1).startswith('./'):
                    continue
                with self.subTest(workflow=name, line=number):
                    # A moving tag can change what a job executes; a commit
                    # cannot. The comment keeps the human-readable version.
                    self.assertRegex(match.group(2), r'^[0-9a-f]{40}$', line)
                    self.assertTrue(match.group(3), f'missing version comment: {line}')
        for name in ('ci.yml', 'structural-perf.yml'):
            text = (ROOT / f'.github/workflows/{name}').read_text()
            with self.subTest(workflow=name):
                self.assertNotIn('actions/checkout@v4', text)
                self.assertIn('actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7', text)
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        for action in ('actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7',
                       'actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1 # v7',
                       'astral-sh/setup-uv@1db35dd8eee11c5894872e0b383ae6c406cf738d # v10.4.0',
                       'actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6'):
            with self.subTest(action=action):
                self.assertIn(action, workflow)
        self.assertNotIn('mozilla-actions/sccache-action', workflow)
        self.assertNotIn('CARGO_INCREMENTAL=0', workflow)
        self.assertNotIn('SCCACHE_CACHE_SIZE', workflow)

    def test_stable_toolchain_and_rust_cache_policy_hold(self):
        toolchain_toml = (ROOT / 'rust-toolchain.toml').read_text()
        match = re.search(r'^channel\s*=\s*"([^"]+)"', toolchain_toml, re.MULTILINE)
        self.assertIsNotNone(match, 'rust-toolchain.toml missing channel')
        pinned_channel = match.group(1)
        ci_text = (ROOT / '.github/workflows/ci.yml').read_text()
        quality_block = ci_text.split('\n  quality:\n', 1)[1].split('\n  release-automation:\n', 1)[0]
        self.assertIn(f'RUSTUP_TOOLCHAIN: "{pinned_channel}"', quality_block)
        self.assertIn(f'toolchain: "{pinned_channel}"', quality_block)
        for job, nxt in (('build', 'windows-build-test'),
                         ('windows-build-test', 'typescript-sdk')):
            block = ci_text.split(f'\n  {job}:\n', 1)[1].split(f'\n  {nxt}:\n', 1)[0]
            with self.subTest(job=job):
                self.assertIn('RUSTUP_TOOLCHAIN: stable', block)
        soak_text = (ROOT / '.github/workflows/soak.yml').read_text()
        self.assertIn('RUSTUP_TOOLCHAIN: stable', soak_text)
        # Only `main`, the default branch, saves Rust caches: every branch and
        # pull request restores `main` entries, `main` cannot restore `dev`
        # ones, and a second per-branch copy overflows the 10 GB cache quota.
        push_save_if = (
            "save-if: ${{ github.event_name == 'push' && "
            "github.ref == 'refs/heads/main' && github.run_attempt == '1' }}"
        )
        expected_save_if = {
            'ci.yml': push_save_if,
            'package-windows.yml': push_save_if,
            'package-compat.yml': push_save_if,
            'structural-perf.yml': push_save_if,
            # The weekly schedule runs on `main`; its push trigger is `dev` only.
            'soak.yml': "save-if: ${{ github.ref == 'refs/heads/main' && github.run_attempt == '1' }}",
            'benchmark-os.yml': "save-if: ${{ github.ref == 'refs/heads/main' }}",
            'loading-ab.yml': "save-if: ${{ github.ref == 'refs/heads/main' }}",
            'windows-smoke.yml': "save-if: ${{ github.ref == 'refs/heads/main' }}",
        }
        for wf, save_if in expected_save_if.items():
            text = (ROOT / f'.github/workflows/{wf}').read_text()
            blocks = text.split('uses: Swatinem/rust-cache@')[1:]
            self.assertTrue(blocks, wf)
            for idx, blk in enumerate(blocks):
                segment = blk.split('\n      -', 1)[0]
                with self.subTest(workflow=wf, idx=idx):
                    if 'save-if: false' in segment:
                        continue
                    self.assertIn(save_if, segment)
                    if wf in ('ci.yml', 'package-windows.yml', 'package-compat.yml',
                              'structural-perf.yml', 'soak.yml'):
                        self.assertIn('cache-on-failure: "false"', segment)

    def test_documentation_only_changes_never_leave_a_required_check_pending(self):
        # A skipped job reports "skipped", which a branch rule waits on forever.
        # The aggregate job always runs and fails only on a real failure, and it
        # must list every other job so a new gate cannot slip past it.
        for name, aggregate in (('ci.yml', 'ci-required'),
                                ('structural-perf.yml', 'structural-perf-required')):
            text = (ROOT / f'.github/workflows/{name}').read_text()
            jobs = text.split('\njobs:', 1)[1]
            names = re.findall(r'^  ([A-Za-z0-9_-]+):\n', jobs, re.MULTILINE)
            block = jobs.split(f'\n  {aggregate}:\n', 1)[1]
            with self.subTest(workflow=name):
                self.assertIn('if: ${{ always() }}', block)
                needs = block.split('runs-on:', 1)[0]
                for job in names:
                    if job != aggregate:
                        self.assertRegex(needs, rf'\b{re.escape(job)}\b', job)
                # An empty or malformed classification must fail the aggregate
                # instead of passing with every gate skipped.
                self.assertIn(".changes.outputs.code // \"\"", block)
                self.assertRegex(block, r'if \[ "\$code" != true \] && \[ "\$code" != false \]; then\n'
                                        r'\s+echo "::error::[^\n]*"\n\s+exit 1\n')
                # The dev push lookup reads the run list with the job's own token.
                changes = re.split(r'\n  [A-Za-z0-9_-]+:\n', jobs.split('\n  changes:\n', 1)[1], maxsplit=1)[0]
                self.assertIn('      actions: read', changes)
                self.assertIn('GH_TOKEN: ${{ github.token }}', changes)
                self.assertIn('BASE_SHA: ${{ github.event.pull_request.base.sha }}', changes)
                self.assertIn('HEAD_SHA: ${{ github.event.pull_request.head.sha || github.sha }}', changes)
                self.assertIn('fetch-depth: 0', changes)
                self.assertIn(f'run: python3 scripts/ci/classify_changes.py --workflow {name} '
                              '--base "$BASE_SHA" --head "$HEAD_SHA"', changes)
        ci_block = (ROOT / '.github/workflows/ci.yml').read_text().split('\n  ci-required:\n', 1)[1]
        self.assertRegex(ci_block, r'if \[ "\$code" != true \] && \[ "\$docs_only" != true \]; then\n'
                                   r'\s+echo "::error::[^\n]*"\n\s+exit 1\n')
        self.assertIn('.value.result != "success" and .value.result != "skipped"', ci_block)
        sp_block = (ROOT / '.github/workflows/structural-perf.yml').read_text().split(
            '\n  structural-perf-required:\n', 1)[1]
        self.assertIn('[ "$perf_result" != success ] && [ "$perf_result" != skipped ]', sp_block)
        # A push to main, a tag, a dispatch and a re-run are never classified,
        # because release evidence needs every mandatory job to have run.
        import importlib.util
        spec = importlib.util.spec_from_file_location(
            'classify_changes', ROOT / 'scripts/ci/classify_changes.py')
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        full = {
            'code': True,
            'docs': False,
            'docs_only': False,
            'rust': True,
            'docs_site': True,
            'ts_sdk': True,
            'py_sdk': True,
            'npm_pkg': True,
            'install_scripts': True,
            'release_scripts': True,
            'workflows': True,
            'tui_only': False,
            'tui_smoke': True,
            'platform_windows': True,
            'platform_macos': True,
            'full_matrix': True,
        }
        for event, ref, attempt in (('push', 'refs/heads/main', 1),
                                    ('push', 'refs/tags/v1.0.0', 1),
                                    ('workflow_dispatch', 'refs/heads/dev', 1),
                                    ('push', 'refs/heads/dev', 2)):
            with self.subTest(event=event, ref=ref, attempt=attempt):
                self.assertEqual(mod.classify(event, ['README.md'], ref=ref, attempt=attempt), full)

    def test_macos_target_reuse_and_cached_tools(self):
        ci_text = (ROOT / '.github/workflows/ci.yml').read_text()
        self.assertIn('scripts/ci/check_warnings.sh --target ${{ matrix.target }}', ci_text)
        self.assertIn(
            'cargo test --locked --target ${{ matrix.target }} -p horon-package-example --test extension_layouts',
            ci_text,
        )
        py_block = ci_text.split('\n  python-sdk:\n', 1)[1].split('\n  docs-site:\n', 1)[0]
        self.assertIn('UV_CACHE_DIR=$RUNNER_TEMP/setup-uv-cache', py_block)
        self.assertIn('enable-cache: false', py_block)
        for cache_key in (
            'actionlint-${{ runner.os }}-v1.7.12',
            'bun-${{ runner.os }}-ts-sdk-',
            'bun-${{ runner.os }}-docs-site-',
            'uv-${{ runner.os }}-py${{ matrix.python }}-',
            'minilm-l6-v2-${{ runner.os }}-v1',
            'direnv-linux-amd64-v2.38.2-148820a187930d36',
            'justrust-${{ runner.os }}-',
            'windows-pip-pywinpty-v1',
        ):
            with self.subTest(cache_key=cache_key):
                self.assertIn(cache_key, ci_text)

    def test_ci_required_gate_rejects_abandoned_result(self):
        # Simulate the exact jq filter used by ci-required to ensure queue-evicted
        # matrix legs (`abandoned`) never count as a green gate.
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        match = re.search(r"bad=\"\$\(echo \"\$NEEDS\" \| jq -r '([^']+)'\)\"", workflow)
        self.assertIsNotNone(match, 'missing strict allowlist filter in ci-required')
        jq_filter = match.group(1)
        out = subprocess.check_output(
            ['jq', '-r', jq_filter],
            input='{"changes":{"result":"success"},"build":{"result":"abandoned"},"docs":{"result":"skipped"}}',
            text=True,
        ).strip()
        self.assertEqual(out, 'build:abandoned')

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

    def test_benchmark_docs_push_through_the_ruleset_without_restarting_ci(self):
        # The ruleset on main and dev requires CI checks and only lets the
        # Horon AI Bot (and admins) push directly. GITHUB_TOKEN pushes would be
        # rejected, and an app-token push without [skip ci] would start CI (and
        # release-tag.yml) on every docs commit.
        workflow = (ROOT / '.github/workflows/benchmark-docs.yml').read_text()
        docs = workflow.split('\n  docs:\n', 1)[1]
        self.assertIn('uses: actions/create-github-app-token@', docs)
        self.assertIn('app-id: ${{ secrets.HORON_AI_BOT_APP_ID }}', docs)
        self.assertIn('private-key: ${{ secrets.HORON_AI_BOT_PRIVATE_KEY }}', docs)
        self.assertIn('token: ${{ steps.bot.outputs.token }}', docs)
        self.assertLess(docs.index('id: bot'), docs.index('actions/checkout@'))
        self.assertIn('\n    permissions:\n      contents: read\n', docs)
        commits = re.findall(r'git commit [^\n]*-m "([^"]*)"', docs)
        self.assertTrue(commits)
        for message in commits:
            self.assertIn('[skip ci]', message)

    def test_pr_os_matrix_scoping_preserves_push_and_shared_platform_gates(self):
        import importlib.util

        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        build_block = workflow.split('\n  build:\n', 1)[1].split('\n  windows-build-test:\n', 1)[0]
        self.assertIn("github.event_name != 'pull_request'", build_block)
        self.assertIn("needs.changes.outputs.platform_macos == 'true'", build_block)
        self.assertIn("needs.changes.outputs.full_matrix == 'true'", build_block)

        win_block = workflow.split('\n  windows-build-test:\n', 1)[1].split('\n  typescript-sdk:\n', 1)[0]
        win_if = next(line.strip() for line in win_block.splitlines() if line.strip().startswith('if:'))
        self.assertEqual(
            win_if,
            "if: ${{ needs.changes.outputs.rust == 'true' && (github.event_name != 'pull_request' || "
            "needs.changes.outputs.platform_windows == 'true' || needs.changes.outputs.full_matrix == 'true') }}",
        )

        # Verify all #[path = "../../platform-linux/src/*.rs"] files included by
        # crates/platform-macos/src/lib.rs set platform_macos=true in classify_changes.py.
        macos_lib = (ROOT / 'crates/platform-macos/src/lib.rs').read_text()
        shared_linux = re.findall(r'#\[path = "\.\./\.\./(platform-linux/src/[^"]+)"\]', macos_lib)
        self.assertEqual(len(shared_linux), 6, shared_linux)

        spec = importlib.util.spec_from_file_location('classify_changes', ROOT / 'scripts/ci/classify_changes.py')
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)

        portable_pr = mod.classify('pull_request', ['crates/extension-types/src/lib.rs'])
        self.assertTrue(portable_pr['rust'])
        self.assertFalse(portable_pr['platform_macos'])
        self.assertFalse(portable_pr['platform_windows'])
        self.assertFalse(portable_pr['full_matrix'])

        for rel in shared_linux:
            path = f'crates/{rel}'
            with self.subTest(shared_macos_path=path):
                res = mod.classify('pull_request', [path])
                self.assertTrue(res['rust'])
                self.assertTrue(res['platform_macos'])
                self.assertFalse(res['platform_windows'])

        labeled = mod.classify('pull_request', ['crates/extension-types/src/lib.rs'], labels={'ci:all-os'})
        self.assertTrue(labeled['platform_macos'])
        self.assertTrue(labeled['platform_windows'])
        self.assertTrue(labeled['full_matrix'])

        for event, ref, attempt in (
            ('push', 'refs/heads/dev', 1),
            ('push', 'refs/heads/main', 1),
            ('workflow_dispatch', 'refs/heads/dev', 1),
            ('push', 'refs/heads/dev', 2),
        ):
            with self.subTest(event=event, ref=ref, attempt=attempt):
                res = mod.classify(event, ['crates/extension-types/src/lib.rs'], ref=ref, attempt=attempt)
                self.assertTrue(res['platform_macos'])
                self.assertTrue(res['platform_windows'])
                self.assertTrue(res['full_matrix'])

    def test_tui_smoke_scoping_and_windows_unified_test_compile(self):
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        self.assertIn("tui_smoke: ${{ steps.classify.outputs.tui_smoke }}", workflow)
        build_block = workflow.split('\n  build:\n', 1)[1].split('\n  windows-build-test:\n', 1)[0]
        for step_name in ('TUI PTY smoke (Linux, macOS)', 'Installed layout TUI smoke'):
            step_block = build_block.split(f'      - name: {step_name}\n', 1)[1].split('\n      - name:', 1)[0]
            self.assertIn("needs.changes.outputs.tui_smoke == 'true'", step_block, step_name)
            self.assertIn("github.event_name != 'pull_request'", step_block, step_name)

        win_block = workflow.split('\n  windows-build-test:\n', 1)[1].split('\n  typescript-sdk:\n', 1)[0]
        for step_name in ('Cache pywinpty wheel on Windows', 'ConPTY smoke (TUI client on Windows)'):
            step_block = win_block.split(f'      - name: {step_name}\n', 1)[1].split('\n      - name:', 1)[0]
            self.assertIn("needs.changes.outputs.tui_smoke == 'true'", step_block, step_name)
            self.assertIn("github.event_name != 'pull_request'", step_block, step_name)

        compile_count = win_block.count("'test', '--locked'") + len(re.findall(r'\bcargo test\b', win_block))
        self.assertEqual(compile_count, 1, 'windows-build-test must compile test targets in a single locked cargo call')
        self.assertIn('--message-format=json-render-diagnostics', win_block)
        self.assertIn('if ($passed -lt $MinPassed)', win_block)
        self.assertIn('$m.Matches[0].Groups[1].Value', win_block)
        self.assertNotIn('$_.Matches', win_block)
        self.assertNotIn('@listArgs', win_block)
        self.assertIn('& $entry.Executable --list', win_block)

        e2e_names = re.findall(r"'((?:provider_behavior|windows_lifecycle)::[A-Za-z0-9_]+)'", win_block)
        self.assertGreaterEqual(len(e2e_names), 4, e2e_names)
        for full_name in e2e_names:
            mod_name, fn_name = full_name.split('::', 1)
            source = (ROOT / f'tests/e2e/{mod_name}.rs').read_text()
            with self.subTest(e2e_test=full_name):
                self.assertIn(f'fn {fn_name}(', source)

    def test_workspace_impact_scoping_and_parallel_integration_targets(self):
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        build_block = workflow.split('\n  build:\n', 1)[1].split('\n  windows-build-test:\n', 1)[0]
        checkout_block = build_block.split('\n      - name: Verify Python eval runner', 1)[0]
        self.assertIn('fetch-depth: 0', checkout_block)
        self.assertIn('scripts/ci/workspace_impact.py', build_block)
        self.assertIn('HORON_CI_SCOPED_PACKAGES=', build_block)
        self.assertIn('if [ "$token" = "horon-agent" ]; then', build_block)
        self.assertIn('${extra_test_targets[@]+"${extra_test_targets[@]}"}', build_block)
        self.assertIn('--jobs 4', build_block)

        joined = re.sub(r'\\\n\s*', ' ', build_block)
        base_calls = [
            m.group(0)
            for m in re.finditer(r'run_lib_tests\.py\s+[^\n]*--package horon-agent-base\b[^\n]*', joined)
        ]
        self.assertEqual(len(base_calls), 2, base_calls)
        for call in base_calls:
            self.assertNotIn(' -- ', call, call)

    def test_structural_perf_unified_compile_and_pr_soak_cycle_split(self):
        ci_text = (ROOT / '.github/workflows/ci.yml').read_text()
        build_block = ci_text.split('\n  build:\n', 1)[1].split('\n  windows-build-test:\n', 1)[0]
        it_step = build_block.split('      - name: Run integration contracts\n', 1)[1].split('\n      - name:', 1)[0]
        self.assertIn(
            "HORON_AGENT_SOAK_CYCLES: ${{ github.event_name == 'pull_request' && '20' || '200' }}",
            it_step,
        )
        soak_text = (ROOT / '.github/workflows/soak.yml').read_text()
        self.assertIn("HORON_AGENT_SOAK_CYCLES: ${{ inputs.cycles || '1000' }}", soak_text)

        sp_text = (ROOT / '.github/workflows/structural-perf.yml').read_text()
        self.assertIn('CARGO_BUILD_RUSTC_WRAPPER: ""', sp_text)
        self.assertIn('apt-get install -y -qq clang mold', sp_text)
        self.assertIn('-fuse-ld=mold', sp_text)
        self.assertIn('CARGO_BUILD_JOBS=', sp_text)

        gate_text = (ROOT / 'scripts/perf/check_structural.py').read_text()
        self.assertIn('"--no-run", "--message-format=json-render-diagnostics"', gate_text)
        self.assertIn('"HORON_AGENT_SOAK_CYCLES": "20"', gate_text)


if __name__ == '__main__':
    unittest.main()
