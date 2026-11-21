#!/usr/bin/env python3
"""Launch two named profiles and prove their state and endpoints stay apart."""
import argparse
import os
import subprocess
import sys
import tempfile


def run(agent, home, runtime, *args):
    env = {k: v for k, v in os.environ.items() if not k.startswith("HORON_")}
    env.update(HORON_HOME=home, HORON_AGENT_NO_AUTO_UPDATE="1", XDG_RUNTIME_DIR=runtime)
    return subprocess.run([agent, *args], env=env, text=True, capture_output=True, timeout=60)


def show(agent, home, runtime, name):
    result = run(agent, home, runtime, "--isolated-profile", name, "--show-profile")
    if result.returncode != 0:
        sys.exit(f"--isolated-profile {name} failed: {result.stderr}")
    return dict(line.split(": ", 1) for line in result.stdout.splitlines() if ": " in line)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--agent", required=True)
    agent = parser.parse_args().agent
    with tempfile.TemporaryDirectory(prefix="hp-", dir="/tmp") as root:
        home = os.path.join(root, "home")
        runtime = os.path.join(root, "run")
        os.makedirs(runtime)
        work, personal = show(agent, home, runtime, "work"), show(agent, home, runtime, "personal")
        assert work["home"] != personal["home"], "profiles share a home"
        assert work["runtime"] != personal["runtime"], "profiles share an endpoint directory"
        assert work["home"] == os.path.join(home, "profiles", "work")
        assert "horon --isolated-profile work" in work["alias"]
        for bad in ("../work", "Work", "work/x"):
            refused = run(agent, home, runtime, "--isolated-profile", bad, "--show-profile")
            assert refused.returncode != 0, f"{bad!r} accepted"
        assert run(agent, home, runtime, "--isolated-profile", "work", "--socket", "/tmp/x.sock", "--show-profile").returncode != 0
        legacy = run(agent, home, runtime, "--show-profile")
        assert legacy.returncode == 0 and "profile: none" in legacy.stdout, legacy.stdout
        os.makedirs(os.path.join(home, "profiles"), exist_ok=True)
        os.symlink(root, os.path.join(home, "profiles", "linked"))
        linked = run(agent, home, runtime, "--isolated-profile", "linked", "--show-profile")
        assert linked.returncode != 0, "a linked profile directory was accepted"
        print("launch profiles are isolated")


if __name__ == "__main__":
    main()
