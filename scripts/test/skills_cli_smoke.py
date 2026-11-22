#!/usr/bin/env python3
"""`skills list` and `skills install` through the real binary and a local git fixture."""
import argparse
import json
import os
import subprocess
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--agent", required=True)
    agent = os.path.abspath(parser.parse_args().agent)
    with tempfile.TemporaryDirectory(prefix="sk-", dir="/tmp") as root:
        home = os.path.join(root, "home")
        runtime = os.path.join(root, "run")
        os.makedirs(home)
        os.makedirs(runtime)
        with open(os.path.join(home, "config.toml"), "w", encoding="utf-8") as handle:
            handle.write("[tools]\nmanaged_skills = true\n")
        env = {k: v for k, v in os.environ.items() if not k.startswith("HORON_")}
        env.update(HORON_HOME=home, HORON_AGENT_NO_AUTO_UPDATE="1", HORON_AGENT_RUNTIME_DIR=runtime)
        managed = os.path.join(home, "managed-skills")

        def run(*args):
            return subprocess.run([agent, "skills", *args], env=env, cwd=root, text=True,
                                  capture_output=True, timeout=120)

        def managed_entries():
            return sorted(os.listdir(managed)) if os.path.isdir(managed) else []

        source = os.path.join(root, "fixture-skill-src")
        os.makedirs(source)
        with open(os.path.join(source, "SKILL.md"), "w", encoding="utf-8") as handle:
            handle.write("---\nname: fixture-skill\ndescription: Fixture skill\n---\n\nBody.\n")
        git = ["git", "-c", "user.email=t@example.com", "-c", "user.name=t"]
        subprocess.run([*git, "init", "-q"], cwd=source, check=True)
        subprocess.run([*git, "add", "."], cwd=source, check=True)
        subprocess.run([*git, "commit", "-q", "-m", "fixture"], cwd=source, check=True)

        # Without approval nothing is written.
        assert run("install", source).returncode != 0, "install without --approve succeeded"
        assert managed_entries() == [], managed_entries()

        # An allowed local source installs and reports its provenance.
        done = run("install", source, "--approve")
        assert done.returncode == 0, done.stderr
        listed = json.loads(run("list", "--json").stdout)
        row = next(item for item in listed if item["name"] == "fixture-skill")
        assert row["provenance"]["source"] == source, row
        assert len(row["provenance"]["commit"]) == 40, row
        assert "source: " + source in run("list").stdout

        # A disallowed source fails and leaves the managed directory as it was.
        before = managed_entries()
        marker = os.path.join(root, "pwned")
        for bad in (f"ext::sh -c touch% {marker}", "http://example.com/x.git", "--upload-pack=x", "/no/such/dir"):
            refused = run("install", bad, "--approve")
            assert refused.returncode != 0, f"{bad!r} was accepted"
        assert not os.path.exists(marker), "a transport source ran a command"
        assert managed_entries() == before, managed_entries()

        # Installing over an existing skill is refused and keeps the original.
        again = run("install", source, "--approve")
        assert again.returncode != 0, "an existing skill was overwritten"
        assert managed_entries() == before, managed_entries()
        print("skills install is gated and leaves no partial skill")


if __name__ == "__main__":
    sys.exit(main())
