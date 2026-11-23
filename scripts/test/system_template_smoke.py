#!/usr/bin/env python3
"""`run --system-template`: bad templates fail before any provider request."""
import argparse
import json
import os
import subprocess
import sys
import tempfile
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
MOCK = os.path.join(ROOT, "scripts", "perf", "mock_llm_server.py")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--agent", required=True)
    agent = os.path.abspath(parser.parse_args().agent)
    server = subprocess.Popen([sys.executable, MOCK], stdout=subprocess.PIPE, text=True)
    try:
        url = json.loads(server.stdout.readline())["url"]
        with tempfile.TemporaryDirectory(prefix="st-", dir="/tmp") as root:
            home = os.path.join(root, "home")
            os.makedirs(home)
            with open(os.path.join(home, "config.toml"), "w", encoding="utf-8") as handle:
                handle.write(
                    f'[provider]\ndefault_provider = "mock"\ndefault_model = "mock-model"\n'
                    f'[providers.mock]\nbase_url = "{url}/v1"\nauth = "none"\n'
                    f'default_model = "mock-model"\nrequires_api_key = false\n'
                    f'[[providers.mock.models]]\nid = "mock-model"\ncontext_window = 128000\n'
                )
            env = {k: v for k, v in os.environ.items() if not k.startswith("HORON_")}
            env.update(HORON_HOME=home, HORON_AGENT_NO_AUTO_UPDATE="1",
                       HORON_AGENT_RUNTIME_DIR=os.path.join(root, "run"))
            os.makedirs(env["HORON_AGENT_RUNTIME_DIR"])

            def requests():
                with urllib.request.urlopen(f"{url}/__stats") as response:
                    return json.load(response)["request_count"]

            def run(template_text):
                path = os.path.join(root, "t.txt")
                with open(path, "w", encoding="utf-8") as handle:
                    handle.write(template_text)
                return subprocess.run(
                    [agent, "run", "--system-template", path, "hi"],
                    env=env, cwd=root, text=True, capture_output=True, timeout=120,
                )

            before = requests()
            for bad in ("{{secret}}", "a {{cwd", "x" * 20000):
                refused = run(bad)
                assert refused.returncode != 0, f"accepted {bad[:12]!r}"
            assert requests() == before, "a refused template still reached the provider"
            ok = run("cwd={{cwd}} date={{date}} model={{model}}")
            assert ok.returncode == 0, ok.stderr
            assert requests() > before, "a valid template sent no request"
            print("system templates are bounded")
    finally:
        server.terminate()


if __name__ == "__main__":
    main()
