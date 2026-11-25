# 성능: 측정 결과와 그 한계

언어: [English](RESULTS.md) | **한국어**

이 페이지는 두 부분으로 구성된다. 위쪽은 최신 `origin/main` 커밋과 경쟁 코딩 에이전트 7종의 최신 안정 릴리스를 Linux·macOS·Windows에서 자동 비교한 결과이고, 아래쪽은 앞서 수행한 동일 기계 A/B 최적화 계획의 과거 기록이다. 서로 다른 계획이나 다른 OS 러너에서 잰 수치는 한 표에서 섞지 않는다.

- **최신 자동 측정(위쪽 절):** [`.github/workflows/benchmark-docs.yml`](../../.github/workflows/benchmark-docs.yml)이 `origin/main` 기준으로 [`benchmark.yml`](BENCHMARK.ko.md)을 Linux(`ubuntu-24.04`), macOS(`macos-latest`), Windows(`windows-latest`)에서 각각 실행하고, 경쟁 도구의 최신 안정 버전(`jcode`는 OS별 최신 릴리스, `Claude Code`·`Codex CLI`·`OpenCode`·`pi`·`omp`·`omo`는 `npm dist-tag latest`)을 매 회차 동적으로 해석한 뒤 `README.md`, `README.ko.md`, [`RESULTS.md`](RESULTS.md), 이 문서, 차트, 문서 사이트 미러를 갱신한다. 현재 기록은 이전 `dev` 브랜치 `v0.1.0-dev` 및 고정 버전 스냅샷을 `origin/main` 커밋 `882db5d7`(`horon-agent v0.1.23-dev (882db5d)`) 기준으로 바꾼 것이다.
- **과거 계획 기록(아래쪽 절):** 기준점 B0 위에서 수행한 `agent-runtime-performance-2` 등 동일 기계 전후 비교 기록을 원본 `record-*.svg` 추세 차트 및 증거 출처와 함께 그대로 보존해, 각 과거 표가 어떤 커밋과 빌드 프로필에서 나왔는지 계속 추적할 수 있게 했다.

## 차트: 최신 자동 측정

<!-- bench-charts:start -->

이 구간은 `.github/workflows/benchmark-docs.yml` 이 아래 자동 측정의 경쟁 도구 비교로 다시 그립니다. 손으로 고치지 않습니다. 모든 차트는 낮을수록 좋고 파란 막대가 horon 입니다.

![벤치마크 요약](charts/00-summary.ko.svg)

| 항목 | 차트 |
| --- | --- |
| `--version` 시간 | [01-version.ko.svg](charts/01-version.ko.svg) |
| 설치 크기 | [02-install-size.ko.svg](charts/02-install-size.ko.svg) |
| 원샷 프롬프트 시간 | [03-oneshot-time.ko.svg](charts/03-oneshot-time.ko.svg) |
| 원샷 프롬프트 메모리 | [04-oneshot-memory.ko.svg](charts/04-oneshot-memory.ko.svg) |
| 스킬 호출 시간 | [05-skill-time.ko.svg](charts/05-skill-time.ko.svg) |
| MCP 도구 호출 시간 | [06-mcp-time.ko.svg](charts/06-mcp-time.ko.svg) |
| 최대 세션 수에서의 메모리 | [07-sessions-memory.ko.svg](charts/07-sessions-memory.ko.svg) |
| 세션 하나당 늘어나는 메모리 | [08-per-session-memory.ko.svg](charts/08-per-session-memory.ko.svg) |
| 원샷 프롬프트당 fsync 호출 수 | [09-fsync-calls.ko.svg](charts/09-fsync-calls.ko.svg) |
| 첫 세션 메모리 | [10-first-session-memory.ko.svg](charts/10-first-session-memory.ko.svg) |

대화형 클라이언트는 별도 바이너리입니다. 이 차트는 `tui` 묶음만으로 그리며 위의 헤드리스 차트와 섞지 않습니다.

| TUI 항목 | 차트 |
| --- | --- |
| TUI 지연 | [tui-01-latency.ko.svg](charts/tui-01-latency.ko.svg) |
| TUI 메모리와 크기 | [tui-02-memory.ko.svg](charts/tui-02-memory.ko.svg) |

<!-- bench-charts:end -->

## 최근 자동 측정의 버전

<!-- bench-versions:start -->

`882db5d7a08096d4d441b674b9865a09e597886b`의 비교 측정을 2026-11-24 23:12 UTC에 실행해 기록한 버전입니다. 여러 러너에서 같은 버전을 기록했으면 한 번만 표시합니다.

| 도구 | 버전 |
| --- | --- |
| horon | `horon-agent v0.1.23-dev (882db5d)` |
| jcode | `jcode v1.1.2 (4f874c9ea)` (Linux), `jcode v1.0.3 (a0ae514c4)` (macOS, Windows) |
| Claude Code | `2.1.331 (Claude Code)` |
| Codex CLI | `codex-cli 0.172.0` |
| OpenCode | `1.19.6` |
| pi | `1.6.0` |
| oh-my-pi (omp) | `omp/19.1.1` |
| oh-my-openagent (omo) | `omo 5.1.83 (engine: senpi 2026.11.23)` |

설치 시점의 npm 레지스트리 메타데이터와 `package.json`에서 검증한 경쟁 도구 출처입니다.

| 도구 | 패키지 | 버전 | 해석 경로 | Git SHA | 무결성 / SHA |
| --- | --- | --- | --- | --- | --- |
| Claude Code | `@anthropic-ai/claude-code` | `2.1.331` | npm dist-tag latest | — | `sha512-E6IE7pon8YAtlzIBB0yhhQgopGvsYuSjnCaoP9ATw64T/f3I4ZfwVcj5jWV9yD7XkrSQZCwO7PgS+qMcP817gA==` |
| Codex CLI | `@openai/codex` | `0.172.0` | npm dist-tag latest | — | `sha512-yHf/6wbw4Zvha2M2ylDhDrU9FgfKLYgV6xSzSjlz3gxQNsiVLkXo+fPGwBqvP0A5geLtZs9wAbq+1v7+ZVWJ0g==` |
| OpenCode | `opencode-ai` | `1.19.6` | npm dist-tag latest | — | `sha512-x0yTvlq3Cyhp3z1IhsNlktLa9M/FSbpMWiDlcGljKMTlBKdHM/um/03JedJa71N2yNlHkOvOPI0H4tbyXuuGwA==` |
| pi | `@earendil-works/pi-coding-agent` | `1.6.0` | npm dist-tag latest | — | `sha512-zX1BVEsshx7A2MRuQ6jfr3RJGWw4Wn+vLq5ZfmlvS1RskhRmByaSZVXH/zcwRrWHO7qPgs98pNCbPOEti/t0fQ==` |
| oh-my-pi (omp) | `@oh-my-pi/pi-coding-agent` | `19.1.1` | npm dist-tag latest | — | `sha512-x/7xQXQ5F/ga4LTNOCSQs+cggWY1H5EF34+juTPP9QOyoFgfmOFVspXA4cBCp9EH8D967oje/HiCPpoNsWAyhw==` |
| oh-my-openagent (omo) | `omo-ai` | `5.1.83` | npm dist-tag latest | `69a1863a457da06fb64ae788e335bf272f1df406` | `sha512-88Q4DT98wRgS1p23alCS96+EFSsWNRyQnYMVga/4Pk9p44mq6JhwDXfDBM5YhFNZ//wnh9rmXRJ0WzEmx689dQ==` |

러너별 jcode 릴리스 신원입니다. 각 작업이 내려받아 검증한 `jcode-release.json`에서 읽습니다.

| OS | 태그 | 커밋 | 자산 | SHA-256 | 제공 경로 |
| --- | --- | --- | --- | --- | --- |
| Linux | `v1.1.2` | `4f874c9ea6b7c3e228045fa8269e4d84bec66a6b` | `jcode-linux-x86_64.tar.gz` | `74ae84a18bb959c7f360ec0a1fecf3eae089ff3f2d92d400062eab4ec32efc56` | release-download |
| macOS | `v1.0.3` | `a0ae514c45451f12d5633ed4fa7c71614a9f4a42` | `jcode-macos-aarch64.tar.gz` | `274396799d2c60600ce3e4890abe78439c1882017eedfc272ce30b1730c5b74b` | release-download |
| Windows | `v1.0.3` | `a0ae514c45451f12d5633ed4fa7c71614a9f4a42` | `jcode-windows-x86_64.exe` | `3c929e46b2facc14a21032f845d26c0e2a80a7a9d008cb64c4845fa4434d30dd` | release-download |

[실행](https://github.com/horon-ai/agent/actions/runs/45506107561)

<!-- bench-versions:end -->

## 여덟 도구 비교 해석

최신 자동 비교([실행 45506107561](https://github.com/horon-ai/agent/actions/runs/45506107561), `origin/main` 커밋 `882db5d7`, 빌드 `horon-agent v0.1.23-dev (882db5d)`, 2026-11-24 23:12 UTC 측정)는 Linux·macOS·Windows 세 OS 모두에서 건너뛴 도구 없이 여덟 에이전트(`horon`과 `jcode`, `Claude Code`, `Codex CLI`, `OpenCode`, `pi`, `omp`, `omo`)를 전부 쟀다.

- **horon이 앞서는 지점:** `--version` 시간은 Linux(`2.9 ms` 대 `jcode` `7.6 ms`)와 Windows(`14.7 ms` 대 `jcode` `16.7 ms`)에서 1위이고 macOS에서는 `jcode v1.0.3`과 사실상 동률이다(`16.0 ms` 대 `15.0 ms`). 원샷 프롬프트 시간은 Linux(`119.1 ms` 대 `jcode` `139.5 ms`)와 macOS(`159.3 ms` 대 `jcode` `436.9 ms`, 최대 RSS `33.3 MB` 대 `36.1 MB`)에서 앞서고, 스킬 호출(`40.6 ms` Linux, `137.5 ms` macOS, `219.5 ms` Windows)과 MCP 도구 호출(`60.4 ms` Linux / 준비 `49.5 ms`, `141.9 ms` macOS / 준비 `107.5 ms`, `253.8 ms` Windows / 준비 `162.7 ms`)은 세 OS 모두에서 7개 경쟁 도구보다 빠르다. 동시 세션에서는 `horon`·`jcode`·`OpenCode`만 데몬 하나에서 세션을 처리해 세션당 추가 프로세스가 `0.00`이고(`horon` 10세션 턴 후 RSS: Linux `54.2 MB`, macOS `41.8 MB`, Windows `36.0 MB`), 세션마다 프로세스를 띄우는 도구(`Claude Code`·`Codex CLI`·`pi`·`omp`·`omo`)는 10세션 기준 Linux `1.3 GB`–`5.3 GB`, macOS `1.1 GB`–`4.1 GB`, Windows `1.0 GB`–`4.1 GB`까지 늘어난다(Windows 10세션에서 `omo`는 연결·steer 오류 2건을 기록했다). 기본 `core` 도구 표면(`oneshot_core`)에서도 첫 모델 요청이 Linux `11,080 B`(`43.9 ms`, RSS `43.6 MB`), macOS `10,921 B`(`139.1 ms`), Windows `10,988 B`(`207.5 ms`)에 머무르며(즉시 도구 8종 + `toolbox` `9,033 B`), 초기 도구 목록이 `18,198 B`–`35,357 B`에 달하는 도구들보다 작다.
- **horon이 뒤지거나 비기는 지점과 구조적 이유:** Linux 원샷 `fsync` 횟수는 정전 내구성을 지키는 `[storage] sync = "full"` 기본값 때문에 `10`회인 반면, 일회성 실행에서 세션 원장을 커밋하지 않는 `Claude Code`(`-p`)와 `pi`(`--no-session`)는 `0`회이고 `Codex CLI` `3`회, `OpenCode` `4`회, `jcode` `5`회, `omp` `25`회, `omo` `38`회다(`AGENTS.md` 규칙상 원본 내구성 플러시는 속도와 맞바꾸지 않는다). Windows 원샷에서는 `jcode v1.0.3`이 시간(`167.6 ms` 대 `204.2 ms`, min–max 범위 `160.1–292.5 ms` 대 `197.7–271.1 ms`로 범위 겹침)과 최대 RSS(`31.6 MB` 대 `36.5 MB`)에서 앞서며, Linux 원샷 최대 RSS는 `horon`(`52.0 MB`)과 `jcode v1.1.2`(`51.7 MB`)가 `same` 수준이다. 세션당 추가 RSS 기울기는 Linux(`63.8 KB` 유휴 / `187.9 KB` 턴 후 대 `109.8 KB` / `528.7 KB`)와 Windows(`97.5 KB` / `272.6 KB` 대 `126.1 KB` / `540.7 KB`)에서 `jcode`가 더 낮고, macOS에서는 유휴 기울기를 `horon`(`141.1 KB` 대 `171.9 KB`)이, 턴 후 기울기를 `jcode`(`310.6 KB` 대 `467.7 KB`)가 앞선다. 한편 업스트림 `jcode v1.1.2`가 macOS·Windows 바이너리를 제공하지 않아 Linux는 `v1.1.2`, macOS·Windows는 `v1.0.3`으로 측정됐다. 설치 크기는 macOS(`105.0 MB`)와 Windows(`107.1 MB`)에서 `horon`이 전체 1위이며, Linux(`124.9 MB`)는 러너에 미리 깔린 Node.js 런타임(`>=22.19`)을 제외한 디렉터리 크기만 재는 `pi`(`117.0 MB`) 다음이다.
- **측정의 한계:** 모든 실행은 실제 API 대신 로컬 모의 모델 서버(`scripts/perf/mock_llm_server.py`)에 붙으므로 기동·IPC·저장·샌드박스·MCP 초기화 비용만 분리해 재며 실제 모델 토큰 지연과 과금은 포함하지 않는다. MCP 연결도 도구별 기본 방식(`horon` `mcp-call`, `jcode` `mcp-action`, `Claude Code` `ToolSearch`, `Codex CLI` 네임스페이스 Responses, `OpenCode`·`pi`·`omo` `mcp-tool` 및 `omo` 콜드 시 백그라운드 `tool_search`, `omp 19.x` `## MCP Tool Commands` 기반 `mcp-command`)을 따르며, OS마다 메모리 단위(`PSS`/`RSS`와 `Working Set`)와 러너 사양이 다르므로 같은 OS 행 안에서만 상대 비교를 읽어야 한다.

## 과거 기준선 기록: `agent-runtime-performance-2` (한눈에 보기)

이 아래 절들은 크로스 OS 자동 비교 전에 수행한 동일 기계 A/B 최적화 계획의 과거 기록을 보존한다. 먼저 `agent-runtime-performance-2` 계획은 기준점 B0(계획 시작 직전 커밋) 위에서 `dev`에 성능 커밋 19개를 올렸고(무관한 `fix(cli)` 커밋 1개는 아래 수치와 무관), 세션 저장소의 해시와 이력 페이지, SSE 파서, 복제 내보내기, 결과 캐시 임계값, attach/detach 소크를 Apple M1 Max(디버그 테스트 프로필)에서 같은 기계 앞뒤 번갈아 재기 방식으로 쟀다. 원본 산출물은 `.omo/evidence/agent-runtime-performance-2/`(`.gitignore:19`)에 있고, 아래 모든 수치는 당시 증거 파일에서 그대로 옮긴 기록이다.

아래 차트가 그 과거 체크포인트의 요약이며, 정확한 수치는 해당 절 표 안에 접힌 채로 있다. 표가 차트 앞을 막지 않게 하려는 것이다.

![세션 저장소: 요청 하나의 이전과 이후](charts/record-01-session-store.ko.svg)

![GitHub Linux 러너의 원샷 프롬프트당 fsync 호출 수](charts/record-05-fsync-trend.ko.svg)

- 이벤트 10만 개 세션에서 이력 페이지 한 장은 62,169,922 B 대신 18,554 B를 읽고, 26.2 s 대신 5.9 ms가 걸린다. 200 MiB 소스 해시의 최고 할당은 268,435,520 B에서 65,600 B로 내려갔다. 시간과 읽은 바이트는 이전과 같다.
- GitHub Linux 러너의 원샷 프롬프트당 fsync 호출은 콜드 40회·웜 35회에서 아래 계획들을 거치며 15회·14회로 줄었다. jcode는 5회에 머무는데, 대화 기록을 커밋하지 않기 때문이다. 이 차이는 조정 대상이 아니라 구조 차이다.
- macOS 원샷은 872.4 ms에서 290.4 ms로, 시작→첫 요청은 236.4 ms에서 78.7 ms로 줄었다. 두 빌드를 같은 기계에서 번갈아 쟀고, 같은 실행에서 jcode 원샷은 294.8 ms였다.
- Linux 원샷은 207.9 ms에서 168.0 ms로 줄었지만 jcode의 146.3 ms보다 여전히 느리다. Windows 격차는 더 크고, 닫혔다고 주장하지 않는다.
- `jcode-parity-optimization` 계획 뒤 세션 하나당 메모리는 Linux에서 149.6 KB로, jcode의 57.4 KB보다 크다. 이전 값은 254.7 KB였다.
- 로딩 계획에서는 벽시계 개선이 확인되지 않았고, A/A 대조가 그 이유를 보여 준다. 같은 바이너리의 두 실행 차이가 그 계획이 주장할 수 있는 어떤 효과보다 크다.

## 측정 결과

차원 표기는 다음 규칙을 따른다. `MEMORY` 는 할당과 메모리, `TIME` 은 벽시계 시간, `WORK` 는 반복 횟수와 바이트량, `RESOURCES` 는 파일 서술자·스레드·태스크 수다. 한 차원의 개선을 다른 차원의 개선으로 읽지 않기 위해 차원을 따로 적는다.

<details>
<summary>정확한 수치</summary>

| 변경 | 차원 | 이전 | 이후 |
| --- | --- | --- | --- |
| 스트리밍 소스 해시, 최고 할당 (200 MiB 로그) | `MEMORY` | 268,435,520 B | 65,600 B (4,092배) |
| 스트리밍 소스 해시, 읽은 바이트 | 변화 없음 | 209,715,200 B | 209,715,200 B |
| 스트리밍 소스 해시, 벽시계 | `TIME` (개선 없음) | 6,434 ms | 6,404 ms |
| 이력 페이지, 평탄화한 메시지 (10만 이벤트, 20개 페이지) | `WORK` | 99,999개 | 20개 |
| 이력 페이지, 읽은 바이트 | `WORK` | 62,169,922 B | 18,554 B |
| 이력 페이지, 벽시계 | `TIME` | 26.2 s | 5.9 ms |
| SSE, 페이로드 12개를 파싱한 횟수 | `WORK` | 24회 | 12회 |
| 복제 내보내기, 직렬화 횟수 (페이지 64 → 256 이벤트) | `WORK` | 129회 | 513회 (이벤트 4배에 직렬화 4.0배) |
| 복제 내보내기, 참조 구현 대비 바이트 (같은 64 → 256) | `WORK` | 8,445,218 → 138,506,722 B (16.4배) | 102,903 → 411,795 B (4.0배) |
| 캐시 facade, 상한 초과 경로 (일시 할당 · 정규 직렬화) | `MEMORY` | 3,146,158 B, 1회 | 0 B, 0회 |
| 캐시 facade, 벽시계 | `TIME` (개선 없음) | MiB당 약 19 ms | MiB당 약 19 ms |
| attach/detach 소크, 200 사이클 뒤 자원 | `RESOURCES` | fd 14, 스레드 25, 태스크 2,141 | fd 14, 스레드 22, 태스크 2,141 |

</details>

몇 가지는 표만으로는 오해하기 쉬워서 따로 적는다.

- 소스 해시의 다이제스트는 다섯 픽스처(61 KB, 618 KB, 6.2 MB, 62 MB, 200 MiB)에서 모두 이전과 같다. 바뀐 것은 최고 할당 하나이고, 읽는 바이트와 걸리는 시간은 그대로다.
- 이력 페이지의 `WORK` 와 `TIME` 은 같은 요청의 두 얼굴이다. 10만 이벤트 세션에서 20개짜리 페이지 하나를 받을 때, 이전에는 세션 전체를 재생해 99,999개 메시지를 평탄화하고 62 MB 를 읽었고, 이후에는 색인이 가리키는 20개 레코드만 읽는다.
- 복제 내보내기는 페이지 크기가 4배가 될 때 이전 참조 구현이 바이트를 16.4배로 늘렸고, 지금 구현은 4.0배로 늘린다. 즉 로그 길이가 아니라 페이지 길이에 비례한다. 페이지 바이트는 B0 과 바이트 단위로 같고, 고정 다이제스트 `135cf07d...`(길이 240,882)와 `4a4855d4...`(길이 26,979)가 그대로 재현된다.
- 캐시 facade 는 상한을 넘는지 판단하려고 정규 JSON 을 한 번 직렬화하던 것을 없앴다. 1 MiB 경계는 바이트 단위로 정확하다(1,048,575 B 는 캐시, 1,048,577 B 는 건너뜀). 실제로 캐시하는 경로는 여전히 한 번 직렬화한다.
- 소크는 200 사이클, 10개 창, 588.99초다. 창마다 증가분을 재고, 실패 라운드와 정지 실패(`quiescence_misses`), 반납 실패(`non_returning`)가 모두 비어 있다.

### 같은 페이지를 로그가 커져도 같은 비용으로 받는가

색인이 있는 상태에서 20개짜리 페이지를 받을 때, 로그가 100배 커져도 읽는 바이트는 0.5%만 늘었다. 재생한 메시지는 세 크기 모두 0이다.

![로그가 100배 커져도 이력 페이지 한 장 값은 그대로](charts/record-02-page-cost.ko.svg)

<details>
<summary>정확한 수치</summary>

| 이벤트 수 | 로그 크기 | 페이지가 읽은 바이트 | 벽시계 |
| --- | --- | --- | --- |
| 1,000 | 514,853 B | 15,375 B | 4.28 ms |
| 10,000 | 5,168,853 B | 15,415 B | 4.68 ms |
| 100,000 | 51,888,853 B | 15,455 B | 4.80 ms |

</details>

이 표의 로그 크기는 성장 테스트가 직접 만든 세션이라 앞 표의 10만 이벤트 픽스처(62,155,848 B)와 크기가 다르다. 같은 측정이 아니다.

### 표에 담지 않은 커밋

나머지 커밋은 앞뒤 쌍보다 상한, 측정 표면, 결정을 바꾼 것이다. NDJSON 프레이밍을 UTF-8 경계에서 안전하게 하고 프레임 상한을 넣은 커밋, 훅의 stdin 쓰기를 데드라인 안으로 옮긴 커밋, 도구 지연을 execute 가 아니라 dispatch 전체로 넓힌 커밋이 있다. 이벤트 스트림에 개수·바이트 상한과 보이는 overflow 를 넣은 커밋, 압축 결정 전의 컨텍스트 복제를 없앤 커밋, 브리지 쓰기를 select 루프 밖 유계 큐로 옮긴 커밋도 있다. 문법 오류로 실행되지 않던 벤치마크 스크립트를 고치고 고정 픽스처 생성기를 넣은 커밋, 단계 카운터·할당 프로브·같은 기계 A/B 실행기를 넣은 커밋, p99 키를 항상 남기는 규칙을 넣은 커밋이 측정 표면을 바꿨다. 네 가지 구조 항목을 측정으로 기각한 결정 기록, 구조 게이트·리포트 템플릿·A/B 안내를 넣은 커밋 둘, 색인 헤더 검사의 중복 클로저를 정리한 후속 수정이 나머지다.

## 측정했지만 채택하지 않은 것

여섯 항목은 측정한 뒤 채택하지 않기로 결정했다. 각 항목에는 그 결정을 뒤집으려면 넘어야 할 숫자가 함께 있다.

| 항목 | 결정 | 결정한 숫자 |
| --- | --- | --- |
| 비활성 세션 축출 정책 | 채택 안 함 | 세션당 fd 0.999개, 스레드 0.998개, RSS 0.41 MB. 1개에서 1,000개까지 이 기울기가 꺾이지 않는다. debug job 수(`server:memory`가 내보내는 `job_count`)는 다섯 인구 수 모두에서 0이고, 소크에서 세션 수는 0으로 돌아온다 |
| `globalEvents` 다중화 | 채택 안 함 (프로토콜 변경) | 감시 세션당 소비자 쪽 fd 7.4개와 스레드 3.0개. 데몬 자신의 몫은 세션당 연결 1.0개이고, 브리지는 연결 하나에 세션 하나를 붙인다 |
| 세션 맵 잠금 범위 축소 | 채택 안 함 | 무관한 세션 제거가 import 전체를 기다린다. 1 / 20 / 60 / 150 이벤트에서 861 / 5,561 / 11,255 / 24,499 ms, 비율 1.00 |
| 인터럽트 폴 25 ms 타이머를 통지로 교체 | 채택 안 함 | 최악 29.7 ms(평균 27.6 ms)를 아끼려고, 감시 그룹 라운드당 wakeup 1회라는 비용 구조를 바꾸는 것이다 |
| `run.stopped` 저널 쓰기와 닫기 저장 겹치기 (#144, #129) | 채택 안 함 (더 느림) | 같은 잡에서 `dev`와 비교한 Windows 마지막 요청→종료 중앙값: 5회 39.8 → 56.4 ms(run 42759006826), 20회 35.2 → 42.8 ms(run 42764256757, IQR 33.0–40.9 대 40.7–45.5). Linux와 macOS는 동률이었다. 이후 #146이 닫기 저장의 flush 자체를 없애 같은 구간이 32.5 ms가 됐다 |
| Windows 고정 셸 환경을 프로세스당 한 번만 계산 (#124, #122) | 채택 안 함 (동률) | Windows 도구 왕복 구간, 20회: 74.3 → 73.5 ms(run 42735692366), min–max 범위 안이다. 왕복의 대부분은 호출마다 만드는 env 표나 PATH 탐색이 아니라 `cmd.exe` 기동 비용이다 |

숫자로 기각한 것은 실패가 아니다. 이 계획은 "개선이 안 나오면 그 항목은 '채택 안 함'으로 근거와 함께 끝냅니다. 그것도 성공입니다"를 성공의 정의로 삼았다. 위 표의 마지막 열이 그 근거다.

## 이 숫자들이 말하지 않는 것

이 절이 이 페이지의 목적이다. 아래 문장 중 하나라도 빠지면 이 페이지는 읽는 사람을 잘못된 결론으로 데려간다.

- **소스 해시의 개선은 해시 단계에만 해당하며, 저장 경로는 검증도 흘려서 한다.** `crates/base/src/session/event_commit.rs:120-133` 는 잡아 둔 접두부를 64 KiB 버퍼 하나로 해시하고, `:148-166` 은 예약된 꼬리를 같은 크기 버퍼로 비교한다. 그래서 큰 로그를 저장해도 로그 사본을 통째로 할당하지 않는다. 계획 당시 기록한 7 KB 로그에서 156,518 B, 553 KB 로그에서 8,293,580 B는 최고 할당이 로그 크기를 따르던 저장에서 나온 값이며 지금 이 경로를 설명하지 않는다.
- **이력 페이지의 개선은 머리가 바뀌지 않은 동안의 반복 페이징에만 해당한다.** 색인은 로그의 신원 스탬프(장치, inode, 길이, mtime, ctime)에 묶여 있어 append 한 번, import 한 번, reset 한 번으로 무효가 된다. 그래서 계속 자라는 세션은 예전 비용을 한 번 더 치르고 색인을 다시 만든다. 그 재구축 비용은 측정되지 않았다. 새 머리에서 페이지를 한 번만 받는 경우는 빨라지지 않는다.
- **캐시의 개선은 할당이지 처리량이 아니다.** 벽시계는 MiB당 약 19 ms 로 양쪽이 같다. 이스케이프 비용이 어느 쪽이든 지배한다.
- **소크는 fd, 스레드, 큐, 대기 요청, 태스크를 단언한다. RSS 는 아니다.** RSS 는 68.4 MB 에서 79.5 MB 로 오른 뒤 평평하게 머물렀고(최고 83.7 MB), 관찰값으로만 남았다.
- **200 사이클 소크는 20 사이클당 자원 1개의 누수를 판별한다.** 계획이 요구한 1,000 사이클은 이제 예약 워크플로 `Attach/Detach Soak`(`.github/workflows/soak.yml`)가 매주 실행하고 첫 실행이 깨끗했으며, 풀 리퀘스트용 형식은 같은 테스트를 구조 게이트에서 20 사이클로 돌리는 것이다(`scripts/perf/check_structural.py:136-146`).
- **이 계획이 끝났을 때 구조 게이트는 프레임 상한을 단언하지 않았다.** 그때 게이트가 읽던 카운터에 프레임 계수가 없었고, 프레임 상한은 `crates/harness-api-server/src/framing_tests.rs` 만 지켰다. 이제 게이트에 `frame` 구획이 있다(`scripts/perf/check_structural.py`). 브리지 리더와 SDK 리더가 각각 `max_frame_bytes`, `accepted_frame_bytes`, `rejected_frame_bytes` 를 보고하고, 게이트는 상한이 `frame_max_bytes`(64 MiB)와 같고 거부된 가장 작은 프레임이 수락된 가장 큰 프레임보다 정확히 1바이트 큰지 확인한다. 프레임 개수는 여전히 세지 않는다.
- **이 숫자들이 덮던 `shared_context` 맵은 이제 트리에 없다.** 토픽 채널 저장소와 함께 제거됐으므로(#299) 소크도 이 맵을 더 이상 표집하지 않고, 위 표에도 이 항목의 값이 없다. 과거 측정값은 기록대로 남긴다. 모든 인구 수에서 0이었고, 그 0은 "누수가 없다"가 아니라 "아무도 쓰지 않았다"는 뜻이었다. `server:memory` 페이로드에 남은 것은 그 옆의 swarm 블록이다. `member_count`, `status_counts`, `swarm_count`, `swarm_membership_count`, `plan_count`, `plan_item_count` 와 각 바이트 추정치이고(`crates/app-core/src/server/debug_server_state.rs`), 앞으로 소크가 표집할 대상은 이 블록이다.
- **복제 내보내기의 직렬화 계수는 페이지 단위로 고정된다.** 256개 상한에서 513회지만, 로그가 커지면 내보내기 시간은 여전히 커진다(1,000 이벤트 278.2 ms, 10,000 이벤트 2,657.2 ms). 바이트가 평평한 것은 페이지 안에서지 전체 내보내기에서가 아니다.
- **캐시 facade 의 워밍 복원 시간은 단발 측정이다.** 페이지 캐시 분산을 포함하고, 같은 케이스에서 레인의 108.6 / 901.8 ms 와 리드의 재실행 108.9 / 499.7 ms 가 그 폭이다. 바이트 단위 경계 판정만 재현 가능한 수치다.
- **네 가지 채택 안 함 결정의 절대 밀리초는 릴리스 수치가 아니다.** 디버그 테스트 프로필과 임시 홈에서 잰 값이고, 전이되는 것은 비율(대기 시간이 import 시간과 같다, 세션 생성 약 1.2 s)이다.
- **`globalEvents` 의 fd 7.4개와 스레드 3.0개는 소비자 쪽 비용이다.** SDK 와 데몬이 같은 프로세스에서 돌아 양 끝이 함께 세어졌다. 100 세션과 1,000 세션 수치는 이 세션당 값에 대한 산술이지 두 번째 측정이 아니다.

## 재현 방법

측정마다 `CARGO_TARGET_DIR` 을 자기 전용 디렉터리로 두고, 한 번에 하나씩만 돌린다. 하나의 타깃 디렉터리에 테스트 바이너리 둘이 동시에 붙으면 매번 다른 실패가 나고, 그 실패는 코드에 대한 정보가 아니다.

```bash
export CARGO_TARGET_DIR="$PWD/.target-measure"
```

측정용 로그는 저장소의 생성기로 만든다. 아래는 200 MiB 로그와 10만 이벤트 로그다.

```bash
python3 scripts/perf/fixtures/gen_event_log.py --bytes 209715200 --seed 20261021 --out /tmp/event-log-200mib/
python3 scripts/perf/fixtures/gen_event_log.py --events 100000 --seed 20261021 --out /tmp/event-log-100000/
```

소스 해시는 프로브 예제가 전체 읽기와 스트리밍 다이제스트를 나란히 인쇄한다.

```bash
cargo build --locked -p horon-agent-base --example source_hash_probe
"$CARGO_TARGET_DIR/debug/examples/source_hash_probe" hash /tmp/event-log-200mib/events.jsonl
```

이력 페이지는 10만 이벤트 픽스처를 환경 변수로 넘겨 재고, 커밋된 500레코드 기준선과의 동일성은 별도 테스트가 본다.

```bash
HISTORY_PAGE_PERF_FIXTURE=/tmp/event-log-100000/events.jsonl \
  cargo test --locked -p horon-agent-harness-api-server --lib history_page_perf -- --nocapture --test-threads=1
cargo test --locked -p horon-agent-harness-api-server --lib pages_match_the_committed_pre_change_baseline -- --nocapture
```

SSE 파서는 청크 크기 네 가지에서 같은 이벤트 열과 파싱 횟수를 인쇄한다.

```bash
cargo test --locked -p horon-agent-provider-openai --lib stream::stream_tool_tests::chunked_replay_yields_the_same_events_at_every_chunk_size -- --nocapture
```

복제 내보내기는 페이지 바이트 동일성과 상한을 저장소 안 테스트가 검증한다. 표에 적은 직렬화 계수(129 / 513)를 인쇄한 QA 하네스는 `.omo/evidence/agent-runtime-performance-2/qa_task14.py` 에 있고 추적되지 않으므로, 클론한 사람은 그 계수를 직접 재현할 수 없다. 다이제스트와 길이는 재현된다.

```bash
cargo test --locked -p horon-agent-session-store replication -- --nocapture
```

캐시 facade 는 테스트가 경계 양쪽의 직렬화 횟수를 인쇄하고, 예제가 할당 최고점을 인쇄한다.

```bash
cargo test --locked -p horon-agent-base --lib event_cache -- --nocapture
cargo build --locked -p horon-agent-base --example facade_cache_probe
```

소크는 200 사이클 기본형과 `#[ignore]` 된 1,000 사이클 변형이 있다. 1,000 사이클형은 예약 워크플로 `Attach/Detach Soak`(`.github/workflows/soak.yml`)가 매주 실행한다. 이 워크플로는 `CI Required` 에 들어가지 않으므로 자원 잔류 실패가 풀 리퀘스트를 막지 않고, 추적 이슈 하나를 만들거나 갱신한다.

```bash
HORON_AGENT_WORKSPACE_TRUST=trust \
  cargo test --locked -p horon-agent --test e2e soak_attach_detach::soak_attach_detach_returns_every_counter_to_baseline -- --exact --nocapture
HORON_AGENT_WORKSPACE_TRUST=trust \
  cargo test -p horon-agent --test e2e soak_attach_detach_1000_cycles -- --ignored --exact --nocapture
```

예약 실행은 직접 재현할 수 있다: `gh workflow run soak.yml --ref dev -f cycles=1000`. 리포트는 `soak-attach-detach-report` 아티팩트이고, `python3 scripts/perf/check_soak.py <캡처한 로그>` 가 로컬 실행에서 같은 마크다운을 만든다.

### 호스트 러너에서 잰 1,000 사이클 기준선

첫 실행은 워크플로가 병합된 뒤 `dev` 푸시가 띄운 것이다([실행 43600180217](https://github.com/horon-ai/agent/actions/runs/43600180217), 커밋 `af41e0b7`). `ubuntu-24.04`, `HORON_AGENT_PERF_COUNTERS=1` 환경이다.

| 측정 | 값 |
| --- | --- |
| 사이클 | 1,000, 100개씩 10개 창 |
| 사이클 소요 | 7분 38초(13:34:30Z → 13:42:08Z), 사이클당 약 0.46초 |
| 잡 전체 | 콜드 빌드를 포함해 10분 56초 |
| 판정 | `STRUCTURAL_PERF section=soak cycles=1000 fd_retention=0 thread_retention=0 non_returning=0` |
| 리포트 | **Clean.** 모든 카운터가 기준선으로 돌아왔다 |

| 카운터 | 기준선 → 최종 | 100 사이클당 기울기 |
| --- | --- | --- |
| `fd_count` | 16 → 16 | +0.000 |
| `thread_count` | 16 → 16 | +0.000 |
| `queue_depth_high_water` | 0 → 1 | +0.100 |
| `pending_requests`, `interrupt_poll_wakeups`, `background_tasks`, `live_sessions`, `connected_clients` | 0 → 0 | +0.000 |

`queue_depth_high_water` 는 최종값이 아니라 최고 수위다. 어느 시점에든 관측된 가장 깊은 큐를 기록하므로 순간적인 1은 잔류가 아니다. 잡 타임아웃 180분은 측정된 실행보다 한 자릿수 이상 여유가 있다.

구조 게이트와 A/B 실행기의 사용법, 리포트 템플릿의 필수 키, 경계를 올리는 절차는 [`docs/performance/BENCHMARK.ko.md`](BENCHMARK.ko.md) 에 있다. 게이트 명령은 `python3 scripts/perf/check_structural.py` 이고, A/B 는 `scripts/perf/ab_perf.py` 다.

## 상태

계획의 항목은 17개이고, 14개가 완료, 3개가 부분 완료(`[~]`)다. 부분 완료의 뜻은 다음과 같다.

| 항목 | 상태 | 뜻 |
| --- | --- | --- |
| 7. 두 SDK 의 NDJSON 프레이밍 상한 | `[~]` | 코드와 테스트는 `dev` 에 있고 검증됐다. 이 항목이 지목한 증거 파일이 워크트리와 함께 사라졌고 재구성하지 않았다. 동작은 증명됐고 증거 궤적은 아니다 |
| 12. 보조·결과 캐시 바이트 회계 | `[~]` | 코드는 `dev` 에 있고, 결정적인 줄은 리드가 직접 재실행해 재현했다. 레인의 원본 증거 파일은 사라졌고, 지금 그 자리에 있는 파일은 스스로 `recovered` 라고 밝힌 재구성본이다 |
| 15. attach/detach 소크 | `[x]` | 계획 항목인 1,000 사이클이 이제 돈다. `.github/workflows/soak.yml` 이 매주·수동으로 실행하고, 첫 실행이 1,000 사이클을 7분 38초에 깨끗하게 마쳤다(`fd_retention=0`, `thread_retention=0`, `non_returning=0`, 실행 43600180217). 200 사이클 형은 풀 리퀘스트 게이트로 남되, 구조 게이트가 20 사이클로 돌린다(`scripts/perf/check_structural.py:136-146`) |
| 17. CI 구조 게이트와 리포트 템플릿 | `[~]` | 게이트는 실제로 돈다(경계 42개 `scripts/perf/structural_budget.json`, `EXIT=0`, 위반 시 `EXIT=1`, 측정 누락 시 `EXIT=2`, 계약 테스트 84개 `scripts/perf/test_check_structural.py`). 계획의 두 조건이 남았다. 프레임 계수 카운터가 없고, "B0 에서 통과"는 구조적으로 불가능하다(`STRUCTURAL_PERF` 줄을 인쇄하는 코드가 B0 뒤에 들어왔다). 이제 게이트는 `frame` 구획에서 두 리더의 프레임 상한 자체를 단언하고, 프레임 개수는 여전히 읽지 않는다 |

나머지 13개 항목은 완료다.

## `agent-rust-loading-durable-runtime` 계획의 측정 (별도 계획, 별도 기준)

위 표는 `agent-runtime-performance-2` 계획의 기록이다. 아래는 뒤에 실행된
`agent-rust-loading-durable-runtime` 계획이 남긴 것이며, 기준 커밋도 측정 도구도 다르다.
두 계획의 수치를 한 표에서 비교하지 않는다.

이 계획의 원본 산출물은 `.omo/evidence/agent-rust-loading-durable-runtime/` 아래에 있다.

### 시간 개선: 확인되지 않음

측정 도구(`scripts/perf/bench_loading.py` 와 `crates/base/examples/loading_probe.rs`)가 이 계획의
산출물이라, 계획 시작 직전의 기준 커밋에는 같은 도구가 없다. 그래서 서로 다른 두 측정
프로그램을 비교하는 대신, 같은 바이너리의 두 설정을 30회씩 교대 측정하는 대조(A/A)를
썼다(`aa_control: true`). 결과는 `improvement_claim: false`다.

| 구간 | A p50 / p95 (ms) | B p50 / p95 (ms) |
| --- | --- | --- |
| attach_history | 671.2 / 1579.2 | 691.7 / 2047.0 |
| provider_dispatch | 12.0 / 26.5 | 13.3 / 66.2 |
| resume_ready | 309.9 / 733.9 | 299.2 / 675.1 |
| session_save | 906.8 / 1271.5 | 993.2 / 1412.7 |
| skill_catalog | 0.2 / 0.2 | 0.2 / 1.4 |

같은 바이너리의 두 실행 사이 편차가 이 계획이 주장할 수 있는 효과보다 크고, 방향도 한쪽으로
치우치지 않는다. 따라서 **벽시계 시간 개선은 주장하지 않는다.**

### 구조 계수: 측정되고, 상한이 지켜짐

시간 대신 계수로 판정하는 항목이다. 두 종류의 증거가 있고, 서로 다른 것을 잰다.

- `python3 scripts/perf/check_structural.py` 가 exit 0 으로 **상한 45개 전부 통과**(실패 0, 누락 0)했다. 이 게이트가 실제로 읽는 계수는 큐 깊이·대기 요청 수·이력 페이지 비용·해시 버퍼·
  프레임 상한·attach/detach 소크 여섯 묶음이다.
- 아래 표의 항목은 그 게이트가 읽지 **않는다**. 각 항목의 이름 붙은 테스트가 직접 단정한다.

| 항목 | 차원 | 무엇이 바뀌었나 | 고정하는 테스트 |
| --- | --- | --- | --- |
| 저장 검증 | `MEMORY` | 커밋 검증이 파일 전체를 할당하지 않고 고정 64 KiB 버퍼로 스트리밍. 소스 길이는 패스 수만 늘림 | `hash_stream` |
| 요청 스냅샷 | `WORK` | 같은 요청에서 메시지 깊은 복사가 이력 길이를 따라 늘지 않음 | `request_snapshot_reuse` |
| 스킬 목록 | `WORK` | 요청당 overlay 스캔 1회 이하, 섹션 상한 49,152 B(UTF-8) | `skill_snapshot_budget` |
| 연결 준비 | `WORK` | core 밖 내장 도구 생성자 호출 0회, 시작 경로가 prewarm 네트워크를 기다리지 않음 | `loading_deferred_setup` |
| 세션 재개 | `WORK` | 살아 있는 세션 재로드와 재개 중복 저장 제거 | `loading_live_resume` |

`MEMORY` 와 `TIME` 을 섞지 않는다. 위 표의 어느 항목도 벽시계 개선을 주장하지 않는다.

### 이 계획이 측정하지 않은 것

- 릴리스 프로필 수치. 모든 측정은 디버그 프로필이다.
- 다중 기기·운영 규모의 처리량.
- provider 호출의 속도. provider 네트워크 시간은 이 계획의 범위가 아니다.

## `agent-irc-bus` 계획의 측정

위 두 기록과 별개로 실행한 `agent-irc-bus` 계획의 측정이다. 이 계획은 에이전트 사이의 메일박스 버스와 `irc` 도구를 넣었고, 기준 커밋 B0는 `50eed12`(`dev`의 계획 시작 직전 커밋), 후보는 `c9d9d8c`(웨이브 3 병합)이다. 앞의 두 기록과 이 기록의 수치를 서로 비교하지 않는다.

원본 산출물은 `.hyper/qa/agent-irc-bus/11-happy.txt`(A/B 보고서 JSON, 처리량 3회, 구조 게이트 `irc` 구역)와 `.hyper/qa/agent-irc-bus/11-failure.txt`(A/A 대조)에 있고, `.hyper/`는 저장소에서 무시되므로 이 절이 추적되는 기록이다.

### 측정 조건

| 항목 | 값 |
| --- | --- |
| 기계 | Apple M1 Max, 10 코어, 메모리 64 GiB |
| 커널 | `Darwin Alpoxs-MacBook.local 25.4.0 ... RELEASE_ARM64_T6000 arm64` |
| 툴체인 | `rustc 1.99.1`, `cargo 1.99.1` |
| 빌드 프로필 | release, 기능 집합 없음(양쪽 모두 `cargo build --locked --release -p horon-agent-base --example perf_counters_surface`) |
| 워크로드 | `perf_counters_surface` 한 번 실행(실행마다 프로세스를 새로 띄움) |
| 측정 중 부하 | 부하 평균 50.10 / 44.73 / 44.59(1분 / 5분 / 15분), `rustc`·`cargo` 프로세스 4개 |
| 반복 수 | 워밍업 3회, 각 쪽 10회, 두 쪽을 번갈아 실행 |
| B0 바이너리 | `48c69b5c...`, 커밋 `50eed12` |
| 후보 바이너리 | `a82fb191...`, 커밋 `c9d9d8c` |

측정 중 이 계획과 무관한 컴파일이 여러 저장소에서 돌고 있었다. 그동안 관측한 부하 평균은 36에서 64 사이였고 `rustc`와 `cargo` 프로세스 수가 4에서 17 사이로 관측됐다. 그래서 이 절의 절대 시간은 조용한 기계의 값이 아니다.

### A/B: 회귀 없음

`scripts/perf/ab_perf.py`가 exit 0으로 끝났고 채택 판정은 `not-adopted-no-measurable-change`다. 회귀를 뜻하는 `reject-regression`이 아니며, 중앙값 차이가 잡음 대역 안이라 개선도 회귀도 주장하지 않는 값이다.

| 항목 | B0 | 후보 | 차이 |
| --- | --- | --- | --- |
| 중앙값 | 5.23 ms | 5.39 ms | +0.16 ms (+3.09%) |
| p95 | 7.06 ms | 7.10 ms | +0.05 ms |
| 잡음 대역(`max(3*1.4826*MAD, 중앙값의 1%)`) | | | ±1.35 ms, 대역 안 |
| `cpu_user_ms` 중앙값 | 2.22 ms | 2.18 ms | -0.04 ms |
| `cpu_system_ms` 중앙값 | 1.55 ms | 1.60 ms | +0.05 ms |
| `peak_rss_bytes` 중앙값 | 6,045,696 B | 6,029,312 B | -16,384 B |
| `fd_count` / `thread_count` 중앙값 | 3 / 1 | 3 / 1 | 같음 |
| 실패 / 시간 초과 | 0 / 0 | 0 / 0 | |
| `read_bytes` 중앙값 | 0 B | 0 B | 같음 |

실행기는 워크로드가 보고하지 않은 `allocated_bytes`, `pss_bytes`, `retained_bytes`를 0이 아니라 `null`로 두고 `counts.missing`에 세 이름을 올렸다. p99는 표본 10개라 20개 미만이라는 이유와 함께 거절됐다.

### A/A 대조: 거짓 회귀 없음

같은 후보 바이너리를 양쪽에 넣은 대조 실행은 `control-no-change`로 끝났다. 중앙값은 6.48 ms와 6.70 ms로 0.22 ms(3.43%) 차이가 났지만 잡음 대역 ±1.58 ms 안이다. 즉 위 A/B의 0.16 ms 차이는 이 측정기가 구분할 수 있는 크기가 아니며, 실행기가 회귀를 지어내지 않는다는 증거다.

### IRC 처리량

`cargo run --locked --release -p horon-agent-irc-core --example irc_throughput`을 3회 실행했다. 메시지 수는 기본값 100,000이고 `broadcast64`는 64의 배수로 내림해 99,968건이다.

![한 프로세스 안 IRC 버스 처리량](charts/record-03-irc-throughput.ko.svg)

<details>
<summary>정확한 수치</summary>

| 시나리오 | 1회차 | 2회차 | 3회차 | 중앙값 | 범위 |
| --- | --- | --- | --- | --- | --- |
| `direct_waiter` (msg/s) | 105,291 | 107,434 | 111,523 | 107,434 | 105,291 ∼ 111,523 |
| `direct_deliver` (msg/s) | 2,410,858 | 2,229,008 | 2,392,230 | 2,392,230 | 2,229,008 ∼ 2,410,858 |
| `broadcast64` (msg/s) | 3,237,934 | 3,141,178 | 3,141,375 | 3,141,375 | 3,141,178 ∼ 3,237,934 |

</details>

`direct_waiter`는 수신 작업 64쌍(`WAITER_PAIRS = 64`)이 동시에 `wait`로 기다리는 구조다. 각 수신 작업은 첫 라운드 뒤부터 상대에게 답장 의무를 만드는 메시지를 한 번 보내고 나서 `wait`에 들어가고, 드라이버는 64개 대기자가 모두 등록된 것을 확인한 뒤 살아 있는 쌍마다 메시지 하나를 보낸다. 쌍 안의 왕복은 순서대로지만 64쌍은 동시에 진행한다. `direct_deliver`는 즉시 주입을 돌려주는 가짜 전달 경로, `broadcast64`는 64개 대상에게 한 본문을 보내는 경로다.

todo 6이 처음 잰 `direct_waiter`는 39,128 msg/s(`6-happy.txt`, 2,555,678 us에 100,000건)였고, todo 12가 `wait` 호출마다 붙던 타이머 왕복을 없앤 뒤 이번 측정에서 107,434 msg/s이다. 약 2.75배다. todo 12가 남긴 측정은 124,071 msg/s(`12-happy.txt`)이고, 이번 값은 그보다 낮다. 측정 시점의 부하 차이로 보이며 코드 경로는 같다.

### 구조 게이트 `irc`

`python3 scripts/perf/check_structural.py --only irc`가 exit 0으로 다섯 경계를 모두 통과했다.

| 카운터 | 관측값 | 경계 |
| --- | --- | --- |
| `mailbox_high_water` | 100 | 100 이하 |
| `mailbox_dropped` | 900 | 900과 같음 |
| `broadcast_body_allocations` | 1 | 1과 같음 |
| `waiters_live_after_churn` | 0 | 0과 같음 |
| `broadcast_inflight_high_water` | 16 | 16 이하 |

### 이 숫자들이 말하지 않는 것

- **A/B 워크로드는 버스가 아니라 프로세스 시작과 카운터 인쇄를 잰다.** `perf_counters_surface`는 데몬을 띄우지 않고 카운터 스냅샷을 한 번 인쇄한다. IRC 버스는 기본 설정에서 꺼져 있고 이 경로에 들어오지 않으므로, 이 A/B가 말하는 것은 "공용 코드에 회귀가 없다"까지다. 버스 자체의 비용은 처리량 표가 답한다.
- **처리량은 같은 프로세스 내부 측정이다.** 예제는 `IrcBus`와 가짜 디렉터리·가짜 전달자를 한 프로세스에 두고 잰다. 데몬 전송, 직렬화, 세션 상태 전이, 모델 호출은 포함하지 않는다. 실제 에이전트 대화의 응답 시간은 이 수치로 예측할 수 없다.
- **`direct_waiter` 수치는 기계 부하에 민감하다.** 이번 측정의 3회는 105,291에서 111,523 사이이고, todo 6의 39,128(`6-happy.txt`)과 견주면 약 2.75배다. todo 12가 남긴 124,071(`12-happy.txt`)과 이번 값의 차이는 측정 시점의 부하 차이로 보이므로, 옮겨 읽을 값은 절대 처리량보다 비율이다.
- **A/B의 절대 시간은 조용한 기계의 값이 아니다.** 측정 중 관측한 부하 평균이 36에서 64 사이였고, 무관한 저장소의 `cargo test`, `cargo clippy`, `cargo build`가 함께 돌았다. A/B가 두 쪽을 번갈아 실행하고 A/A가 잡음 대역을 확인했으므로 비교의 방향은 유효하지만, 5 ms대 절대값은 다른 조건에서 재현되지 않는다.
- **p99는 없다.** 표본이 10개라 실행기가 20개 미만을 이유로 거절했다.
- **세 지표는 측정되지 않았다.** `allocated_bytes`, `pss_bytes`, `retained_bytes`는 워크로드가 보고하지 않아 `null`이고 `counts.missing`에 있다. 0으로 읽지 않는다.
- **구조 게이트는 시간을 재지 않는다.** 위 다섯 값은 모양이 상한 안에 있는지만 말하고, 빠른지는 말하지 않는다.

## `platform-adapter-perf` 계획의 측정

위 세 기록과 별개로 실행한 `platform-adapter-perf` 계획의 측정이다. 이 계획은 OS별 코드를 `crates/platform-*` 다섯 크레이트로 모으고, 세션 저장에서 다시 만들 수 있는 파생 캐시의 fsync를 없애고, HTTPS 클라이언트의 TLS 설정을 프로세스당 한 번만 만들고, 가격표 새로고침을 기본으로 끈다. 기준 커밋 B0는 `738712a`(`dev`의 계획 시작 직전 커밋), 후보는 `215bfdb`(작업 브랜치)이다. 앞 세 기록과 이 기록의 수치를 서로 비교하지 않는다.

원본 산출물은 `.hyper/qa/platform-adapter-perf/after/` 아래에 있다. `.hyper/`는 저장소에서 무시되므로 이 절이 추적되는 기록이다.

| 파일 | 내용 |
| --- | --- |
| `compare-macos.json`, `compare-linux.json`, `compare-linux-times.json` | 세 줄(horon B0, horon 후보, jcode) 비교. Linux `compare-linux.json`은 `--syscalls`, 시간 수치는 `compare-linux-times.json`에서 |
| `fsync-linux.txt` | one-shot 한 건의 fsync·fdatasync 호출 수, cold와 warm |
| `ab-*.json` | 같은 기계 A/B 보고서(실행기 출력 JSON 그대로) |
| `github-baseline/`, `github-candidate/` | GitHub `benchmark.yml` 실행 42294971747(`--ref dev`)과 42294976581(브랜치)의 아티팩트 |
| `structural.log` | `check_structural.py` 전체 출력 |

### 측정 조건

| 항목 | macOS | Linux | GitHub 러너 |
| --- | --- | --- | --- |
| 기계 | Apple M1 Max, 10 코어 | Docker `rust:1-trixie`, linux/aarch64, 10 코어 | `ubuntu-latest`, linux/x86_64, 4 코어 |
| 툴체인 | `rustc 1.99.1` | 컨테이너 배포판 rustc | `stable` (실행 시점) |
| 프로필 | release | release | release |
| 한 측정 안의 두 바이너리 | 같은 기계, 번갈아 실행 | 같음 | 다른 실행, 다른 러너 인스턴스 |
| 경쟁 도구 | jcode v0.99.1 (`7206efd0c`) | 같음 | 같음 |

한 측정 안의 모든 줄이 같은 모의 모델 서버에 붙으므로 API 요금과 시크릿은 쓰지 않는다. macOS와 Linux는 B0 바이너리와 후보 바이너리를 같은 실행 안에서 쟀고, GitHub은 `dev` 실행과 브랜치 실행을 따로 띄웠다. 그래서 아래 표에서 GitHub 열만 "전후"가 아니라 "두 실행"이다.

### 세 환경 전후

one-shot은 `run` 질문 한 건(기동, `ls` 도구 왕복 1회, 최종 응답)의 중앙값이다. start→first request는 api-bridge 소켓 연결부터 첫 `list_sessions` 응답까지다. fsync는 one-shot 한 건이 부른 fsync·fdatasync 호출 수다. 메모리는 데몬 프로세스 트리의 idle RSS이다.

![macOS·Linux 시작 경로: 이전과 이후](charts/record-04-platform-time.ko.svg)

<details>
<summary>정확한 수치</summary>

| 지표 | 환경 | B0 `738712a` | 후보 `215bfdb` | 변화 | jcode | 후보 / jcode |
| --- | --- | --- | --- | --- | --- | --- |
| one-shot 중앙값 | macOS | 872.4 ms | 290.4 ms | -66.7% | 294.8 ms | 0.99× |
| one-shot 중앙값 | Linux (Docker) | 207.9 ms | 168.0 ms | -19.2% | 146.3 ms | 1.15× |
| one-shot 중앙값 | GitHub 러너 | 185.3 ms | 499.8 ms | 별도 실행, 비교 불가 | 145.5 ms / 357.8 ms | 비교 불가 |
| start→first request | macOS | 236.4 ms | 78.7 ms | -66.7% | 해당 없음 | 해당 없음 |
| start→first request | Linux (Docker) | 57.0 ms | 57.0 ms | -0.03% | 해당 없음 | 해당 없음 |
| start→first request | GitHub 러너 | 23.7 ms | 25.3 ms | 별도 실행 | 해당 없음 | 해당 없음 |
| fsync·fdatasync 호출 | Linux (Docker) | 175 cold / 170 warm | 40 cold / 35 warm | -77% | 19 cold / 5 warm | 7.0× |
| fsync·fdatasync 호출 | GitHub 러너 | 잴 수 없음 | 40 cold / 35 warm | 잴 수 없음 | 19 / 10 / 5 회 | 3.5× |
| 첫 세션 idle RSS | macOS | 32.73 MiB | 29.52 MiB | -9.8% | 37.34 MiB | 0.79× |
| 첫 세션 idle RSS | Linux (Docker) | 32.05 MiB | 33.37 MiB | +4.1% | 40.29 MiB | 0.83× |
| 세션당 증가분 | macOS | 592 KiB | 320 KiB | -46% | 544 KiB | 0.59× |
| 세션당 증가분 | Linux (Docker) | 1396 KiB | 944 KiB | -32% | 144 KiB | 6.6× |
| 첫 세션 idle RSS | GitHub 러너 | 잴 수 없음 | 39.9 MiB | 잴 수 없음 | 43.0 MiB | 0.93× |
| 세션당 증가분 | GitHub 러너 | 잴 수 없음 | 319 KiB | 잴 수 없음 | 38 KiB | 8.3× |

</details>

이 표의 후보 열은 `215bfdb` 바이너리로 잰 값이다. 그 뒤 브랜치에 쌓인 변경 가운데 시작 경로를 건드린 것은 지연 HTTP 클라이언트(`b6dcfc0`) 하나이고, 시간·메모리 재측정은 아래 "메모리" 절에 있다. F2 품질 검토가 찾은 11건의 수정도 이 수치를 바꾸지 않았고, 같은 실행기로 다시 재면 Linux fsync가 cold 40 · warm 35로 값이 같다(`F2-review.md`의 "fsync" 절).

fsync의 40 상한 판정은 `bench_compare.py --syscalls` 실행기가 잰 값(cold 40 · warm 35)이다. todo 6의 자체 드라이버는 HOME과 프롬프트 모양이 달라 같은 후보 바이너리에서 cold 45 · warm 40을 쟀고, 그 실행에서는 40 상한을 warm만 만족한다(`6-linux-fsync.txt`의 "result against the plan").

호스트 러너의 절대값은 기계마다 달라 뜻이 없으므로 GitHub 열은 "워크플로가 브랜치에서 그대로 돌고 fsync 수도 기록된다"까지만 읽는다. GitHub B0 실행의 세션 측정은 세 지점 모두 `daemon socket not ready`로 실패했다. 그 실행은 `dev`의 `bench_compare.py`를 쓰는데, `serve --debug-socket` 제거에 맞춘 수정(todo 13)이 아직 브랜치에만 있어서다. 그래서 GitHub B0의 세션 값은 0으로 읽지 않는다.

### 같은 기계 A/B: 시간 경로에 회귀 없음

`scripts/perf/ab_perf.py`는 두 바이너리를 블록 단위로 번갈아 실행하고 잡음 대역 `max(3*1.4826*MAD, 중앙값의 1%)`을 함께 낸다. 대역을 넘는 차이만 개선이나 회귀로 부른다.

한 줄이 한 경로다. one-shot과 start→first request, 세션 경로는 준비(임시 HOME, 모의 모델 주소, 데몬 기동)를 대신 해 주는 작은 프로브를 거치므로 이 표의 절대값은 앞 환경 표보다 프로브 기동분만큼 크다. 두 쪽이 같은 프로브를 쓰므로 비교 방향은 그대로다.

<details>
<summary>정확한 수치</summary>

| 경로 | 환경 | B0 | 후보 | 차이 | 잡음 대역 | 판정 |
| --- | --- | --- | --- | --- | --- | --- |
| one-shot | macOS | 1121.85 ms | 435.79 ms | -61.2% | ±93.95 ms | 개선 |
| start→first request | macOS | 236.36 ms | 78.72 ms | -66.7% | ±10.36 ms | 개선 |
| 세션 경로 2개 | macOS | 3295.26 ms | 2259.88 ms | -31.4% | ±131.13 ms | 개선 |
| `--version` 로드 | macOS | 7.76 ms | 7.52 ms | -3.0% | ±1.36 ms | 대역 안 |
| `perf_counters_surface` | macOS | 4.74 ms | 4.80 ms | +1.3% | ±1.41 ms | 대역 안 |
| one-shot | Linux | 265.02 ms | 217.62 ms | -17.9% | ±14.61 ms | 개선 |
| `--version` 로드 | Linux | 2.60 ms | 2.63 ms | +1.2% | ±0.39 ms | 대역 안 |
| `perf_counters_surface` | Linux | 0.46 ms | 0.47 ms | +1.6% | ±0.08 ms | 대역 안 |
| start→first request | Linux | 57.03 ms | 57.02 ms | -0.03% | ±2.93 ms | 대역 안 |
| 세션 경로 1개 | Linux | 14882 ms | 14836 ms | -0.3% | ±148.82 ms | 대역 안 |
| 세션 경로 2개 | Linux | 14936 ms | 14858 ms | -0.5% | ±149.36 ms | 대역 안 |

</details>

도구 지연과 기동 단계는 같은 기계에서 따로 확인했다. `bench_tools.py`의 14개 케이스를 3회씩 번갈아 돌린 결과 중앙값 차이는 -15.7%에서 +5.4% 사이이고, 커진 케이스는 `ls`(+5.4%), `agentgrep`(+1.1%), `read (repo Cargo.toml)`(+0.9%)이다. `ls`는 B0의 세 회차가 2.21 / 2.46 / 3.57 ms이고 후보가 2.58 / 2.59 / 2.64 ms라 B0 자신의 회차 폭 안에 들어간다. `bench_startup.py`의 서버 준비 중앙값은 10.8∼10.9 ms(B0)와 11.0 ms(후보), 브리지 총 시간은 18.1∼18.6 ms와 17.9∼18.6 ms로 양쪽이 겹친다. GitHub 후보 실행에서 변경과 무관한 도구 케이스까지 20∼40% 높았던 것은 러너 인스턴스 차이이고, 같은 기계에서는 재현되지 않는다.

macOS가 크게 줄어든 것은 이 계획이 노린 두 비용이 macOS에만 있었기 때문이다. 파생 캐시를 포함한 저장마다 `F_FULLFSYNC`를 걸던 경로가 원본으로 줄었고, reqwest 클라이언트마다 키체인 인증서를 읽던 경로가 프로세스당 한 번 생성으로 바뀌었다. 대조로 `--version` 로드와 `perf_counters_surface`는 두 환경 모두 대역 안이다. 이 두 경로는 저장도 TLS도 지나지 않으므로, 공용 코드에 회귀가 생겼다면 여기서 먼저 보였을 자리다.

### 메모리: Linux 데몬 상주량의 처음 수치와, 다시 잰 뒤의 결론

시간 경로는 어디서도 느려지지 않았다. 메모리는 Linux 데몬 상주량(`rss`)이 늘어난 것으로 처음 기록했는데, 그 측정 방법 자체가 두 빌드를 비교할 수 없다는 것을 뒤에 확인했다. 처음 수치를 먼저 남기고, 재측정과 원인 수정을 그 아래 적는다.

<details>
<summary>정확한 수치</summary>

| 측정 | 환경 | B0 | 후보 | 차이 |
| --- | --- | --- | --- | --- |
| 데몬 idle, 세션 0개 | macOS | 27.82 MiB | 24.09 MiB | -3.73 MiB (-13.4%) |
| 데몬 idle, 세션 1개 | macOS | 32.34 MiB | 28.95 MiB | -3.39 MiB (-10.5%) |
| 데몬 idle, 세션 0개 | Linux | 27.88 MiB | 29.01 MiB | +1.13 MiB (+4.1%) |
| 데몬 idle, 세션 1개 | Linux | 31.74 MiB | 32.75 MiB | +1.01 MiB (+3.2%) |
| one-shot 최고 RSS (8회) | Linux | 32.46 MiB | 32.87 MiB | +0.41 MiB (+1.3%, 범위 겹침) |
| 같은 바이너리 대조 (A/A) | Linux | 27.83 MiB | 27.87 MiB | +0.03 MiB (+0.1%) |

</details>

Linux의 차이는 힙이 아니라 실행 파일 상주 페이지다. 세션 1개 상태의 `/proc/<pid>/smaps`를 매핑 종류별로 더하면 다음과 같다.

| 매핑 | B0 | 후보 | 차이 |
| --- | --- | --- | --- |
| 실행 파일 `r-xp` | 26176 KiB | 27136 KiB | +960 KiB |
| 실행 파일 `r--p` | 1508 KiB | 1508 KiB | 0 |
| 익명 `rw-p` | 1796 KiB | 1730 KiB | -66 KiB |
| 그 밖 | 1784 KiB | 1784 KiB | 0 |

같은 측정을 한 번 더 돌리면 `r-xp` 차이는 +960에서 +1472 KiB, 익명 차이는 -66에서 +156 KiB 사이에서 나온다. 두 번 모두 방향은 같고 크기는 실행 파일 쪽이 압도적이다. 세션이 없을 때도 같은 방향으로 `r-xp` +1152 KiB, 익명 +10 KiB이다. 보조 페이지 폴트 중앙값은 1574와 1584로 사실상 같고 주 페이지 폴트는 양쪽 0이다. 바이너리는 276,928 B 커졌고 `.text`는 100,576 B 커졌으므로, 늘어난 상주량은 파일 크기보다 크게 나온다. 즉 더 많은 코드 페이지가 실제로 닿는다. 세션당 증가분은 반대로 B0 1396 KiB에서 944 KiB로 줄었다.

#### 다시 잰 결과: 이 차이는 코드가 아니라 페이지 캐시였다

처음 수치는 같은 파일을 양쪽에 쓰는 A/A 대조만 했기 때문에 "이 바이너리 하나에서는 측정이 반복된다"만 보증한다. 같은 소스를 독립적으로 두 번 빌드해 같은 방법으로 재면 그 차이보다 큰 폭으로 흔들린다.

| 대조 | rss 중앙값 | `r-xp` 중앙값 |
| --- | --- | --- |
| 같은 커밋, 빌드 1 vs 빌드 2 | 22.22 vs 21.63 MiB | 22068 vs 22028 KiB |
| 같은 커밋, 빌드 1 vs 빌드 3 | 22.22 vs 26.33 MiB | 22068 vs 21948 KiB |
| 후보 커밋, 빌드 1 vs 빌드 2 | 27.00 vs 23.17 MiB | 22576 vs 22716 KiB |
| 같은 바이트, 경로만 다름 | 14.39 vs 26.85 MiB | 11276 vs 24076 KiB |

원인은 `r-xp` 상주 페이지가 실행한 코드가 아니라 커널이 폴트마다 함께 매핑한 페이지 캐시 폴리오라는 점이다. 상주 구간의 중앙 길이는 32∼48페이지(64 KiB 폴리오)이고, 실제 실행 구간은 1∼2페이지다. 그래서 같은 소스의 두 빌드도 상주량이 8 MiB까지 갈린다.

측정을 캐시에 흔들리지 않는 값으로 바꿨다. callgrind 명령어 덤프(`--dump-instr=yes`)에서 실행한 4 KiB 코드 페이지 수를 세고, `rss`는 시작 전마다 `posix_fadvise(DONTNEED)`로 페이지 캐시를 비우고 `fault_around_bytes`를 4096으로 고정해 쟀다.

| 빌드 | 실행 코드 페이지 | `rss` 중앙값(통제 측정) |
| --- | --- | --- |
| 기준선 738712a #1 | 1765 | 26.42 MiB |
| 기준선 #2 | 1765 | 26.37 MiB |
| 후보 f5f5868 #1 | 1771 | 26.92 MiB |
| 후보 #2 | 1778 | - |
| 후보 #3 | 1771 | - |

한쪽 안의 빌드 간 폭(기준선 1759∼1765)이 양쪽 차이(1765 vs 1771)와 같거나 크다. 즉 처음 기록한 "+1.0∼1.3 MiB 실행 코드 상주"는 이 변경에 귀속되는 회귀가 아니다.

#### 실제로 고친 것: 시작할 때 TLS 신뢰 저장소를 만들던 경로

`serve`는 시작할 때 도구 레지스트리를 만든다. 그때 `WebFetchTool::new`, `WebSearchTool::new`, `DiscoverToolsTool::new`, `NotificationDispatcher::new`가 각각 `shared_http_client()`를 들고 있었고, 그 클라이언트를 만드는 일은 신뢰 저장소 449개 앵커를 전부 파싱하는 일이다.

    shared_http_client -> ClientBuilder::build -> openssl_probe::probe
      -> rustls_native_certs::load_pem_certs
      = 시작 instructions의 83%, 실행 코드 페이지 878개 중 194개

네 생성자가 클라이언트를 들지 않게 바꿨다. `shared_http_client()`는 이미 프로세스 전역 `OnceLock`이므로, 요청을 보내는 지점에서 그대로 부르면 된다.

| 쪽 | 실행 코드 페이지 | `rss` 중앙값 | `r-xp` 중앙값 |
| --- | --- | --- | --- |
| 기준선 | 1765 | 26.42 MiB | 22068 KiB |
| 수정 전 후보 | 1771 | 26.92 MiB | 22576 KiB |
| 수정 후 후보 | 1755 | 26.59 MiB | 22208 KiB |

수정 후 실행 코드 페이지는 기준선보다 적는다(1755 vs 1765). 남은 `rss` 차이 +0.17 MiB는 위에서 본 같은 소스 빌드 간 폭(통제 측정에서 최대 8 MiB, 같은 쌍에서 0.6 MiB) 안이며, 실행 코드 차이로는 나타나지 않는다.

시작 경로의 나머지는 양쪽이 같다. idle까지의 시스템 호출은 5416 vs 5428(53종 중 46종 동일), 연 파일은 364 vs 365이다.

### 구조 게이트

`python3 scripts/perf/check_structural.py`가 exit 0으로 55개 경계를 모두 통과했다(실패 0, 누락 0). 이 게이트는 벽시계 시간을 읽지 않으므로 위 시간 수치와는 다른 축을 지킨다.

### 이 숫자들이 말하지 않는 것

- **GitHub 러너의 두 실행은 전후 비교가 아니다.** 서로 다른 호스티드 러너 인스턴스이고, 브랜치의 워크플로만 `--syscalls`를 켜므로 후보 실행에만 strace가 붙는다. 후보 실행에서 이 변경과 무관한 바이너리까지 일제히 느려졌다: jcode 145.5 → 357.8 ms, Claude Code 452.1 → 913.9 ms, Codex CLI 369.4 → 2273.1 ms, OpenCode 2.98 → 5.81 s, pi 928.4 → 2109.5 ms. 바뀌지 않은 실행 파일이 두 배 넘게 느려졌으므로 두 실행의 차이는 이 계획이 아니라 러너와 계측에서 온다. 같은 Docker 기계에서 잰 strace 비용은 one-shot당 +255에서 +259 ms였고, 후보 실행의 변경 없는 도구 지연도 일률적으로 높다(`bash (true)` 2.2 → 3.1 ms, `read (tiny file)` 0.5 → 0.6 ms). 이 계획의 회귀 판정은 같은 기계에서 잰 macOS와 Docker Linux 두 환경으로 한다.
- **macOS의 -66.7%는 이 계획의 두 비용에만 해당한다.** 저장 경로와 클라이언트 생성 경로를 지나지 않는 `--version` 로드와 `perf_counters_surface`는 대역 안이다. 모델 지연, 도구 실행 시간, 프롬프트 조립 시간은 이 수치에 들어 있지 않는다.
- **fsync 호출 수는 Linux 컨테이너에서만 센다.** macOS에는 `strace`가 없고, 이 계획은 macOS의 기본 동기화를 바꾸지 않았다(설정 `[storage] sync = "fast"`로만 일반 fsync).
- **처음 잰 Linux 데몬 RSS +1.0∼1.3 MiB는 회귀가 아니었다.** 그 값은 실행한 코드가 아니라 커널이 폴트마다 함께 매핑한 페이지 캐시 폴리오를 재고 있었다. 같은 소스의 두 빌드도 더 크게 흔들리므로 그 수치로 이 변경을 판정할 수 없다. 캐시에 흔들리지 않는 실행 코드 페이지로 다시 재면 후보가 기준선보다 적고(수정 후 1755 vs 1765), 통제한 `rss` 차이는 같은 소스 빌드 폭 안이다. 대신 시작할 때 TLS 신뢰 저장소를 만들던 경로를 찾아 없앴다(위 "실제로 고친 것").
- **첫 세션 RSS와 세션당 증가분은 다른 축이다.** Linux 후보는 첫 세션 값이 +1.3 MiB 높고 세션당 증가분은 -452 KiB이다. 두 값을 하나의 "메모리"로 합치지 않는다.
- **`perf_counters_surface`는 데몬을 띄우지 않는다.** 이 워크로드가 말하는 것은 "공용 코드에 회귀가 없다"까지이고, 저장 경로나 TLS 경로의 비용은 one-shot 표가 답한다.
- **p99는 없다.** 표본이 12개 이하라 실행기가 20개 미만을 이유로 거절했다.

## `cache-context-session-opt` 계획의 측정

이 계획은 OpenAI 호환 요청에 `prompt_cache_key`와 보존 기간을 싣고(`ee9f46bc`), 같은 입력이면 그 키를 다시 계산하지 않으며(`263ab39e`), 압축 꼬리를 회복 대역(기준의 0.8)에 맞추고(`6e7f5052`), 묶음 저장의 로그 동기화를 묶음 끝 한 번으로 합칩니다(`626eebb3`). 성능은 GitHub Actions에서만 잰다. 후보는 `dev`의 `e95b8773`이고, 측정은 2026-11-07의 `benchmark.yml` 실행 42371020359(`--ref dev`, linux·macos·windows 모두 성공)와 `structural-perf.yml` 실행 42371014382(성공)이다. 비교 기준은 위 `platform-adapter-perf` 절에 기록된 같은 러너 값이다.

| 지표 | 환경 | 이전 기록 | 후보 `e95b8773` | 변화 |
| --- | --- | --- | --- | --- |
| one-shot fsync·fdatasync 호출 (표본 3) | GitHub Linux 러너 | 40 cold / 35 warm | 32 cold / 27 warm | -20% cold, -23% warm |
| 그중 fsync / fdatasync (cold) | GitHub Linux 러너 | 해당 없음 | 16 / 16 | 해당 없음 |
| jcode 대비 배수 (warm) | GitHub Linux 러너 | 7.0× (35 / 5) | 5.4× (27 / 5) | 차이 축소 |
| one-shot 시간 중앙값 | GitHub macOS 러너 | 해당 없음 | 175.52 ms (jcode 341.8 ms) | 해당 없음 |
| one-shot 시간 중앙값 | GitHub Windows 러너 | 해당 없음 | 269.02 ms (jcode 175.37 ms) | 해당 없음 |
| 구조 성능 게이트 | GitHub Linux 러너 | 통과 | 통과 | 회귀 없음 |

줄어든 호출은 로그 줄의 fdatasync이다. 3건 묶음 저장의 `LogFileSync`가 3에서 1로 줄어든 것은 `crates/session-store/tests/batch_sync.rs`가 고정한다.

### 이 숫자들이 말하지 않는 것

- **one-shot 시간은 표본이 3개다.** 이 절은 OS별 현재 값을 적을 뿐이고, 같은 러너의 이전 시간 값과 A/B로 비교한 것이 아니다. 아래 자동 측정 절이 `main`에서 같은 묶음을 계속 다시 잰다.
- **캐시 키의 효과는 여기 없다.** 벤치마크는 가짜 모델 서버를 쓰므로 캐시 적중이 생기지 않는다. 적중은 실제 모델 회사의 `cached_tokens`로만 보다. 키 계산 비용은 세션 안에서 입력이 바뀌지 않으면 비교 한 번으로 끝나도록 메모했다(`crates/provider-openai/src/chat/prompt_cache_memo.rs`).
- **압축 회복 대역은 시간 지표가 아니다.** 24k 창의 남기는 꼬리가 10,800에서 3,625 토큰으로 줄고 200k 창은 140,000 / 20,000 그대로라는 것은 단위 테스트가 확인한다.

## `jcode-parity-optimization` 계획의 측정

이 계획은 기본 실행에서 가격표 읽기와 두 번째 설정 파싱을 건너뛰고(`76a510fd`), 세션마다 저장 스레드를 두던 것을 최대 4개짜리 공용 writer 풀로 바꾸고(`c75dd149`), 더티 표식을 디렉터리 fsync 하나로 확정하고(`af13232d`), 30분 넘게 쉰 세션의 파생 캐시를 놓는다(`1266059d`). 성능은 GitHub Actions에서만 잰다. `benchmark.yml`의 `baseline_ref` 입력으로 기준 `main`(`b62a79c9`)과 후보를 **한 잡 안에서 같은 러너, 같은 가짜 모델 서버로 번갈아** 쟀으므로 아래 이전/이후는 같은 기계의 A/B이다. 실행은 2026-11-08의 42388003895(linux·macos, 세션 1·10·100, 턴 3, one-shot 표본 3)와 42389421614(macos, one-shot 표본 20)이다.

![jcode-parity 계획 뒤 세션 하나당 비용](charts/record-06-per-session-memory.ko.svg)

<details>
<summary>정확한 수치</summary>

| 지표 | 환경 | 이전 `b62a79c9` | 이후 | jcode v0.99.1 |
| --- | --- | --- | --- | --- |
| 세션 하나가 늘 때 메모리 (기울기) | GitHub Linux 러너 | 254.7 KB | 149.6 KB | 57.4 KB |
| 세션 하나가 늘 때 메모리 (기울기) | GitHub macOS 러너 | 249.6 KB | 185.1 KB | 153.4 KB |
| 세션당 스레드 | GitHub Linux 러너 | 1.14 | 0.16 | 0.07 |
| 세션당 스레드 | GitHub macOS 러너 | 1.34 | 0.26 | 측정 불가 |
| 100세션 턴 뒤 트리 RSS | GitHub Linux 러너 | 96.2 MB | 79.2 MB | 65.5 MB |
| 100세션 턴 뒤 트리 RSS | GitHub macOS 러너 | 84.5 MB | 75.4 MB | 70.1 MB |
| one-shot fsync·fdatasync 호출 (cold, 첫 표본) | GitHub Linux 러너 | 32 | 27 | 19 |
| one-shot fsync·fdatasync 호출 (warm, 마지막 표본) | GitHub Linux 러너 | 27 | 22 | 5 |
| one-shot 시간 중앙값 (표본 3) | GitHub Linux 러너 | 367.0 ms | 363.9 ms | 288.7 ms |
| one-shot 시간 중앙값 (표본 20) | GitHub macOS 러너 | 219.3 ms | 215.8 ms | 312.9 ms |

</details>

세션당 fd는 두 환경 모두 이전과 이후가 1.00으로 같다.

### 이 숫자들이 말하지 않는 것

- **표본 3개의 macOS 중앙값은 회귀처럼 보였다.** 42388003895에서 이후 384.3 ms, 이전 254.4 ms였지만 표본이 이후 616·384·245 ms, 이전 594·254·235 ms로 흩어져 있었다. 같은 비교를 표본 20개로 다시 잰 42389421614에서 중앙값 215.8 대 219.3 ms, 최솟값 174.7 대 173.5 ms로 차이가 없었다. 그래서 `benchmark.yml`에 `oneshot_runs` 입력을 두었다.
- **jcode와의 차이는 아직 남아 있다.** Linux에서 세션 하나가 늘 때 메모리는 jcode의 약 2.6배이고 one-shot은 1.26배 느리다. 이 계획은 그 차이를 줄였을 뿐 없애지 않았다.
- **세션당 스레드 기준(기울기 0.1 미만)은 Linux에서만 맞았다.** 세션 1·10·100 세 점의 기울기는 Linux 0.16, macOS 0.26이었다. 세션 500을 더한 42397804493(같은 러너 A/B, one-shot 표본 20)에서는 Linux 0.05(이전 1.04, jcode 0.02), macOS 0.40(이전 1.45)이다. Linux의 세 점 값은 적은 세션 수에서 런타임 스레드가 섞인 잡음이었지만, macOS는 500세션에서도 0.1을 넘는다. writer 풀은 최대 4개이므로 macOS에서 남은 기울기는 다른 곳(데몬의 블로킹 작업 스레드로 보임)에서 오며, 원인은 확인하지 않았다. 500세션 턴 뒤 트리 RSS는 Linux 289.2 → 189.9 MB(jcode 159.9 MB), macOS 226.8 → 161.7 MB(jcode 156.8 MB)이다.
- **macOS one-shot 중앙값은 두 번 잰 결과의 방향이 엇갈린다.** 표본 20개로 42389421614에서는 이후 215.8 대 이전 219.3 ms, 42397804493에서는 199.4 대 188.7 ms(+5.7%)였고 최솟값은 두 번 모두 같았다(174.7 대 173.5, 168.7 대 168.6 ms). 같은 실행의 Linux는 410.6 대 407.6 ms이다. 회귀로 판정하지 않았지만 호스티드 러너의 잡음 대역이 약 ±6%라는 뜻이기도 한다.
- **`ab_perf.py` 경로별 A/B는 돌리지 않았다.** 이 계획의 시간 비회귀 판정은 위 one-shot 표(같은 러너 번갈아 측정)와 구조 성능 게이트로 했고, `perf_counters_surface`·start→first request 같은 경로별 A/B는 기록에 없다.
- **hibernation의 효과는 위 표에 없다.** 벤치마크는 턴 직후 잠깐 쉬고 재므로 30분 창이 지나지 않는다. 해제가 실제로 일어나는지는 `crates/app-core/src/server/idle_hibernation_tests.rs`가 확인한다.

## `skill-mcp-loading-omp` 계획의 측정

스킬 목록, 작업 폴더 스킬, MCP 사용자 설정 로딩을 바꾼 계획이다. 판정은 `.github/workflows/loading-ab.yml` 한 번의 실행으로 했다. 개발 기기에서 잰 수치는 판정에 쓰지 않았다.

### 측정 조건

- 실행: [run 42461323656](https://github.com/horon-ai/agent/actions/runs/42461323656), 2026-11-08, `ubuntu-24.04` 잡 하나에서 두 쪽을 릴리스로 빌드하고 번갈아 잼.
- 기준선 `23007c9b`(로딩 카운터가 처음 들어간 커밋), 후보 `83f2b6c7`. 툴체인 `rustc 1.99.1`.
- `bench_loading.py --mode warm --events 0 --skills 0,200,1000`, 예열 3회, 측정 30회씩. 두 조건: 본문 8 KiB와 `~/.claude.json` 300 KB, 그리고 둘 다 0. 픽스처가 `[mcp] claude_user = true`를 켜서 두 쪽이 같은 파일을 읽는다.
- 측정 단계 하나는 요청 9회분이다(`REPEAT_REQUESTS`).

### 카운터: 세 조건 모두 참

| 픽스처 (본문 8 KiB, MCP 300 KB) | 카운터 | 기준선 | 후보 |
| --- | --- | --- | --- |
| 스킬 200개 | `skill_catalog` `skill_bytes_read` | 1,656,290 | 819,200 |
| 스킬 1000개 | `skill_catalog` `skill_bytes_read` | 8,281,890 | 4,096,000 |
| 스킬 0·200·1000개 | `mcp_config_load` `mcp_config_reads` | 9 | 0 |
| 스킬 0·200·1000개 | `skill_request_overlay` `skill_file_reads` | 0 | 0 |

본문이 0인 조건의 `skill_bytes_read`(23,490 / 117,890)는 두 쪽이 같다. 앞머리만 있는 파일이라 줄일 본문이 없다. `skill_request_overlay`의 파일 읽기는 기준선도 0이다. 기준선은 요청 범위 안에서 같은 목록을 다시 쓰고, 대신 요청마다 폴더를 다시 훑는다. 그 비용은 아래 시간에 나온다.

### 시간 (p50, 괄호는 p95, ms)

![스킬·MCP 로딩: 같은 잡에서 기준선과 후보](charts/record-07-skill-mcp.ko.svg)

<details>
<summary>정확한 수치</summary>

| 단계 | 조건 | 기준선 | 후보 |
| --- | --- | --- | --- |
| `skill_request_overlay` | 스킬 200, 본문 8 KiB | 28.30 (29.09) | 4.79 (4.96) |
| `skill_request_overlay` | 스킬 1000, 본문 8 KiB | 149.03 (153.92) | 26.46 (27.83) |
| `skill_catalog` | 스킬 200, 본문 8 KiB | 4.41 (4.53) | 3.00 (3.11) |
| `skill_catalog` | 스킬 1000, 본문 8 KiB | 21.88 (22.64) | 15.00 (15.32) |
| `skill_catalog` | 스킬 1000, 본문 0 | 15.71 (15.98) | 14.82 (15.28) |
| `mcp_config_load` | MCP 300 KB | 3.46 (3.71) | 0.22 (0.26) |

</details>

`bench_tools.py`의 14개 사례는 모두 후보 중앙값이 기준선과 같거나 작다(표본 5개씩, 같은 잡).

### 측정 중에 고친 것

- 첫 실행([run 42458666340](https://github.com/horon-ai/agent/actions/runs/42458666340))에서 본문 0 조건의 `skill_catalog`가 기준선 p95를 넘었다(스킬 1000개 9.65 → 13.52 ms). 원인은 이름 충돌 처리가 스킬마다 앞서 받은 목록을 처음부터 훑던 것과, 충돌이 없어도 스킬마다 namespace 문자열과 비교 키를 만들던 것이다. `abd7618e`, `83f2b6c7`에서 이름 색인, 충돌 때만 namespace 계산, 스레드 하나 줄이기, 첫 4 KiB 안의 앞머리는 힙 버퍼 없이 파싱하기로 고쳤고, 위 표가 고친 뒤의 결과다.
- 벤치 스크립트의 MCP 픽스처 확인이 "측정 단계에서 파일을 다시 읽었는가"를 보던 것을 "그 파일의 서버가 로드됐는가"로 바꿨다(`b3a053b3`). 파싱 캐시가 바뀌지 않은 파일을 다시 읽지 않는 것이 이번 변경의 목적이기 때문이다.

### 이 숫자들이 말하지 않는 것

- 실행마다 러너가 달라 같은 기준선도 실행 사이에 수 ms씩 움직였다(본문 0, 스킬 1000개 `skill_catalog` 기준선 9.65 / 15.62 / 15.71 ms). 비교는 같은 실행 안의 두 쪽끼리만 했다.
- 콜드 시작(파일 캐시가 빈 상태)은 재지 않았다.
- Windows는 ctime이 없어, 같은 크기로 고치고 수정 시각을 되돌린 스킬 파일은 다음 요청에 보이지 않을 수 있다(`docs/runtime/PROMPT.ko.md`).

## fsync 감축과 jcode와의 구조적 차이 (#47)

세 변경을 넣었다. 명시 선택을 한 번도 하지 않은 세션은 `local-head-explicit.json`에 `false`를 쓰지 않고(A), 세션 writer는 이미 있는 `sessions/` 루트의 부모를 열 때마다 다시 flush하지 않는다(B). 반대로 영속 런타임 저널은 새로 만든 폴더의 항목을 부모에 확정하도록 고쳤다(C). C 전에는 정전 뒤 이미 `Ok`를 돌려준 `runtime.jsonl`이 폴더째 사라질 수 있었다. 계약은 [`../protocol/SESSION_STORE.ko.md`](../protocol/SESSION_STORE.ko.md)에 있다. 측정은 2026-11-09 `benchmark.yml` 실행 42531992327이고, 기준 `dev`(`c3e3ceeb`)와 후보(`d9d2c836`)를 한 잡 안에서 같은 러너로 번갈아 쟀다. one-shot 표본은 5개다.

<details>
<summary>정확한 수치</summary>

| 지표 | 환경 | 이전 `c3e3ceeb` | 이후 `d9d2c836` | jcode v0.99.1 |
| --- | --- | --- | --- | --- |
| one-shot fsync·fdatasync 호출 (warm) | GitHub Linux 러너 | 16 | 15 | 5 |
| one-shot 시간 중앙값 (최소–최대) | GitHub Linux 러너 | 60.5 ms (54.2–133.1) | 57.7 ms (54.3–111.4) | 146.9 ms |
| one-shot 시간 중앙값 (최소–최대) | GitHub macOS 러너 | 98.3 ms (90.3–389.4) | 115.1 ms (96.1–393.0) | 362.4 ms |
| one-shot 시간 중앙값 (최소–최대) | GitHub Windows 러너 | 257.5 ms (235.0–562.6) | 284.8 ms (225.8–468.6) | 146.9 ms |
| 스킬 호출 시간 중앙값 (최소–최대) | GitHub Windows 러너 | 243.3 ms (220.2–260.9) | 264.6 ms (235.0–366.3) | 205.9 ms |
| MCP 호출 시간 중앙값 (최소–최대) | GitHub Linux 러너 | 72.3 ms (70.2–144.0) | 101.8 ms (72.3–288.0) | 169.0 ms |
| MCP 호출 시간 중앙값 (최소–최대) | GitHub Windows 러너 | 287.5 ms (276.9–302.2) | 299.3 ms (277.5–350.7) | 317.8 ms |
| 세션 하나가 늘 때 메모리, 턴 뒤 | GitHub Linux 러너 | 516.7 KB | 404.2 KB | 218.0 KB |
| 세션 하나가 늘 때 메모리, 턴 뒤 | GitHub Windows 러너 | 378.5 KB | 353.8 KB | 324.9 KB |

</details>

시간 칸의 이전·이후 차이는 모두 최소–최대 범위 안에 있으므로 동률로 읽는다. 이 변경은 시간 경로에 flush를 더하지 않고 줄이기만 한다(C의 flush는 저널 폴더를 처음 만들 때 한 번뿐이고 append마다 들지 않는다).

Windows one-shot과 스킬 호출은 이 변경과 관계없이 jcode보다 느리다(#44, #45). 이 표는 그 차이를 줄였다고 말하지 않는다.

### 남는 10회 차이는 구조적 차이다

- jcode의 one-shot fsync는 전부 사용량 통계 SQLite(`model-usage-v1.sqlite3`, WAL)에서 나온다. 대화 기록(세션 저널과 스냅샷)은 `append_json_line_fast`와 `write_json_fast`로 쓰며 flush하지 않는다. 그래서 jcode는 프로세스 충돌에는 안전하지만 전원 차단 시 커널이 아직 내려쓰지 않은 대화를 잃는다.
- horon의 15회는 모두 원본의 확정이다. 이벤트 로그 커밋마다 1회, 세션 생성 때 폴더 항목, 로컬 상태 파일(`local-runtime.json`)과 그 폴더, 영속 런타임 저널과 그 폴더다.
- AGENTS.md 방침에 따라 다음은 넣지 않았다. 턴 끝에 한 번만 flush하는 그룹 커밋(도구 결과 기록이 확정 전 상태로 남음), 비동기 flush 스레드(`Ok` 뒤에 확정되므로 계약 약화), `sync_file_range`와 `O_DSYNC`(내구성이 아니거나 횟수가 늘어남).
- 더 줄이려면 로컬 상태 파일과 이벤트 로그를 한 기록으로 합치는 저장 형식 변경이 필요하다. 별도 설계 과제다.

### 이 숫자들이 말하지 않는 것

- **fsync 호출 수는 Linux에서만 센다.** macOS에는 `strace`가 없다. 세 변경은 macOS `F_FULLFSYNC`와 Windows `FlushFileBuffers`에도 같은 비율로 적용되지만 그 횟수는 재지 않았다.
- **첫 실행(cold)은 1회 늘 수 있다.** C는 `durable-runtime/`을 처음 만들 때 홈 폴더 항목도 확정하므로, 홈마다 한 번 더 든다.

## 닫기 저장과 세션당 메모리: 원인 진단과 구조적 차이 (#44, #45, #47, #48)

Windows 단발 실행·스킬 호출의 열세와 세션당 메모리 증가를 진단 기록으로 나눈 뒤, 데이터 안전을 깎지 않는 수정 하나만 넣었다. 실행 끝의 닫기는 상태만 바꾸는데도, 보조 기록(`AuxiliaryRecorded`)에 상태·pid·마지막 활동 시각(`runtime_history`)이 함께 들어 있어서 매번 이벤트 로그 커밋, 바인딩 확정, 파생 캐시 재작성을 모두 했다. 이 값은 읽는 곳이 없고 원본은 `local-runtime.json`이므로 보조 기록에서 뺐다([`../protocol/SESSION_STORE.ko.md`](../protocol/SESSION_STORE.ko.md)). 측정은 2026-11-09 `benchmark.yml` 실행 42572765871이고, 기준 `dev`(`fca59cd6`)와 후보(`1dd5cd2d`)를 한 잡 안에서 같은 러너로 번갈아 쟀다. 표본은 5개다.

| 지표 | 환경 | 이전 | 이후 | jcode v0.99.1 |
| --- | --- | --- | --- | --- |
| one-shot fsync·fdatasync 호출 (warm) | GitHub Linux 러너 | 15 | 14 | 5 |
| one-shot 마지막 요청 → 종료 (중앙값) | GitHub Linux 러너 | 11.5 ms | 8.2 ms | 4.4 ms |
| one-shot 마지막 요청 → 종료 (중앙값) | GitHub Windows 러너 | 61.8 ms | 46.5 ms | 11.9 ms |
| one-shot 시간 중앙값 (최소–최대) | GitHub Linux 러너 | 61.5 ms (60–63) | 58.7 ms (57–100) | 144.9 ms |
| one-shot 시간 중앙값 (최소–최대) | GitHub Windows 러너 | 255.8 ms (224–303) | 224.3 ms (220–305) | 151.7 ms |
| 스킬 호출 시간 중앙값 (최소–최대) | GitHub Linux 러너 | 57.0 ms (55–60) | 53.4 ms (52–54) | 56.3 ms (55–58) |
| 스킬 호출 시간 중앙값 (최소–최대) | GitHub Windows 러너 | 260.5 ms (256–306) | 250.6 ms (233–263) | 232.7 ms (211–256) |
| MCP 호출 시간 중앙값 (최소–최대) | GitHub Windows 러너 | 301.6 ms (281–363) | 269.8 ms (255–279) | 337.2 ms |

Linux 스킬 호출은 범위가 겹치지 않아 jcode보다 빠르게 되었다. Windows 시간 칸은 이전·이후 범위가 겹치므로 개선 방향으로만 읽는다.

### Windows 종료 구간의 남은 몫

진단 빌드(실행 42568847961, 구간별 시간 로그)로 나눈 Windows one-shot 종료 구간은 최종 응답 저장 15 ms(이벤트 로그 확정 약 8 ms, 파생 캐시 재작성 약 4.5 ms), `run.stopped` 저널 기록 10 ms, 닫기 저장 18 ms, 프로세스 정리 약 4 ms였다. 이번 수정은 닫기 저장만 바인딩 확정 하나로 줄였다. 남은 몫은 아래 이유로 줄이지 않았다.

- 이벤트 로그와 `run.stopped` 저널은 원본이다. Windows `FlushFileBuffers`는 호출마다 수 ms가 들지만 확정을 빼면 정전 시 대화와 실행 상태를 잃는다. jcode는 대화 기록을 확정하지 않으므로 이 차이는 구조적 차이다.
- 커밋마다 다시 쓰는 파생 캐시(약 4.3 ms, flush 없음)는 턴 중간에 붙는 다른 클라이언트가 읽으므로 쓰는 주기를 바꾸지 않았다.
- 도구 왕복의 bash 실행(Windows 약 23 ms)은 벤치마크의 `ls`를 `cmd.exe /D /S /C`로 실행하는 비용이다. 셸을 바꾸지 않고는 줄일 수 없다.
- 시작 구간의 첫 HTTP 클라이언트 생성은 5 ms로, 인증서 저장소 읽기가 원인이라는 추정은 측정으로 기각했다.

### 세션당 메모리 (#48)

실행 42568289247(Linux, 턴 직후, 세션 1·5·10의 기울기)에서 세션 하나가 늘 때 RSS 477 KB 가운데 익명 메모리가 440 KB, 할당자가 살아 있다고 보고한 데이터가 234 KB, `allocator:purge` 뒤 RSS가 417 KB였다. heaptrack(진단 실행 42571121373)으로 세션 1개와 10개를 비교하면 살아 있는 힙이 세션당 약 247 KB 늘고, 가장 큰 증가 지점도 세션 생성 29 KB, 잠근 도구 목록 10 KB, 이벤트 위치 색인 8 KB 정도로 모두 세션이 실제로 쓰는 데이터다. 누수는 없다.

- 반환해도 60 KB만 줄었으므로, 나머지 약 200 KB는 할당자 영역 안의 단편화다. 반환을 더 자주 해도 효과가 작다.
- macOS에서는 살아 있는 데이터 증가가 horon 140 KB로 jcode 191 KB보다 작고, 이번 실행의 Windows 턴 뒤 증가도 horon 310 KB로 jcode 396 KB보다 작다. Linux 차이는 할당자(glibc)의 단편화에서 오며, 할당자를 바꾸는 것은 다른 경로의 회귀 위험이 커서 이번에 하지 않았다.

## `perf-bench-harness-prompt` 계획의 측정

이 계획은 한 턴이 치르는 요청 바이트와 프롬프트 표면을 재고, 줄이고, 구조 게이트로 묶었다(#324). 동작도 하나 바뀐다. 세션 도중 `.horon/prompt-overlay.md`, `preferred-tools.md`, `system-prompt.md`를 고치면 이제 다음 세션 경계(새 세션, `/clear`, 작업 폴더 변경, 복원)에서 반영된다. AGENTS.md가 이미 따르던 규칙과 같다.

### 측정 조건

- 3개 OS A/B: [run 43667470921](https://github.com/horon-ai/agent/actions/runs/43667470921), 후보 `f5993e9d`, 같은 잡에서 `dev` `5285d2a4`로 만든 기준선 `horon-base`. `suites=compare`, `oneshot_runs=5`, 중앙값과 min–max이다. min–max가 겹치는 차이는 동률이다.
- `compare,startup,tools`를 묶은 Linux A/B: [run 43670649131](https://github.com/horon-ai/agent/actions/runs/43670649131).
- macOS `compare` 재측정: [run 43672354272](https://github.com/horon-ai/agent/actions/runs/43672354272). 첫 macOS `--version` 표본이 겹치지 않아 다시 쟀다.
- 바이트와 카운터 값: 구조 게이트 [run 43666931419](https://github.com/horon-ai/agent/actions/runs/43666931419)와 ci.yml [run 43662099986](https://github.com/horon-ai/agent/actions/runs/43662099986)이며 ubuntu와 macOS가 같다. 전 값은 같은 브랜치 Wave 1(`308f6c5e`, 이 표면들에서 `dev`와 같음)에서 쟀다.

### 프롬프트 표면과 세션 컨텍스트

![모델을 부르기 전 한 턴이 보내는 것](charts/record-08-prompt-surface.ko.svg)

<details>
<summary>정확한 수치</summary>

| 표면 | 전 | 후 |
| --- | --- | --- |
| 즉시 보내는 core 도구 카탈로그(스키마 8개) | 9,714 B | 9,151 B |
| core one-shot 첫 요청의 `tools`(모의 서버) | 9,818 B | 9,255 B |
| 정적 프롬프트(core, 프롬프트 파일 없음) | 1,367 B, 319 토큰 | 1,322 B, 305 토큰 |
| `# Additional tools` 색인 | 629 B, 문장 중간에서 잘린 행 5개 | 585 B, 1개(`todo`) |
| 바뀌지 않은 요청 9번의 프롬프트 원천 읽기 시도 | 54 | 0 |
| 세션 컨텍스트 `git` 프로세스, 저장소 밖 / 작업 트리 | 1 / 3 | 0 / 1 |
| PCI 표시 장치가 없을 때 GPU 탐지 프로세스 | 1 | 0 |

</details>

PR #332에서 도구 목록 상한을 9,151 B로 낮췄다. `1dfdd06a`에서 도구 8개에 공통으로 들어가는 `intent` 설명을 줄인 뒤 실측값인 8,975 B를 새 상한으로 정했다. 정적 프롬프트는 1,322 B였고 내용이 같은 요청 9회에서 프롬프트 원본을 읽은 횟수는 0회였다. 같은 Linux 러너에서 첫 요청의 `tools` 필드는 `83da1c39`의 9,255 B에서 9,079 B로 줄었다. 도구 목록은 목표인 9,000 B보다 25 B 작고 첫 요청은 최초 기준인 9,818 B보다 739 B 작다. [구조 측정 결과](https://github.com/horon-ai/agent/actions/runs/44190404248/job/134418534689) · [모의 서버가 받은 첫 요청](https://github.com/horon-ai/agent/actions/runs/44190400494/artifacts/14378517143)

### 시간과 메모리 (중앙값 ms, 괄호는 min–max)

![OS별 원샷·코어·스킬·MCP 시간, 이전과 이후](charts/record-09-prompt-time.ko.svg)

<details>
<summary>정확한 수치</summary>

| OS | 시나리오 | `horon-base` (dev) | 후보 |
| --- | --- | --- | --- |
| Linux | one-shot | 40.24 (39.92–179.35) | 29.35 (28.39–44.31) |
| Linux | core one-shot | 42.92 (41.29–49.03) | 31.31 (30.72–45.41) |
| Linux | skill 호출 | 37.43 (36.77–38.08) | 26.31 (25.22–26.45) |
| Linux | MCP 도구 호출 | 55.62 (50.58–57.65) | 40.16 (39.54–44.96) |
| macOS | one-shot | 54.93 (53.10–495.17) | 47.07 (44.89–265.97) |
| macOS | core one-shot | 60.63 (55.86–88.40) | 49.28 (48.89–127.21) |
| macOS | skill 호출 | 108.27 (85.84–128.53) | 62.82 (51.71–87.13) |
| macOS | MCP 도구 호출 | 82.51 (80.09–191.23) | 76.41 (73.10–206.67) |
| Windows | one-shot | 170.15 (158.82–229.05) | 188.26 (142.17–370.69) |
| Windows | core one-shot | 200.21 (171.71–436.16) | 179.55 (149.18–427.49) |
| Windows | skill 호출 | 176.06 (158.48–180.42) | 145.75 (139.41–231.53) |
| Windows | MCP 도구 호출 | 302.51 (201.68–494.52) | 268.93 (172.28–412.46) |

</details>

- one-shot 최대 RSS는 Linux 46.6 → 39.4 MB, macOS 31.6 → 24.0 MB, Windows 33.9 → 33.6 MB이다. Linux one-shot fsync는 양쪽 모두 10회 그대로다.
- Linux run 43670649131도 같은 방향이다. one-shot 48.69 → 34.59 ms, skill 호출 43.12 → 30.72 ms, MCP 도구 호출 78.07 → 49.12 ms, 최대 RSS 40.0 → 38.3 MB, fsync 10과 10이다.
- min–max가 겹치지 않게 느려진 시나리오는 없다. Windows one-shot(170.15 → 188.26)과 run 43672354272의 macOS MCP 도구 호출(101.94 → 107.63)은 중앙값이 나빠졌지만 범위가 겹치므로 동률이다.
- `--version`: 첫 macOS 표본은 7.36 (6.99–7.50) → 7.89 (7.79–8.18) ms로 겹치지 않았다. 재측정은 9.27 (8.96–10.48) → 8.29 (7.99–8.43) ms로 반대 방향으로 겹치지 않았고, Linux와 Windows는 두 실행 모두 겹친다. 이 브랜치는 `--version` 경로의 코드를 바꾸지 않았으므로 첫 표본은 러너 잡음이다.
- startup·tools 묶음은 같은 잡 기준선이 없어 보고만 한다. `serve` 소켓 준비 20.39 ms(dev 기준 run 43612646876에서 20.57 ms), 브리지 no-op 응답 21.59 ms(21.89 ms)이다.

### 이 숫자가 말하지 않는 것

- 시간 단축의 원인은 나누지 않았다. 세션 컨텍스트의 `git` 프로세스 감소와 요청당 프롬프트 작업 감소가 모두 측정 경로에 있지만, 그 차이를 둘로 나눈 실행은 없다.
- `[features] foreign_rules`(기본값 꺼짐)는 여전히 요청마다 규칙 파일을 읽고, 프롬프트 원천 읽기 수에 잡히지 않는다.
- Windows 벤치 빌드는 기준선과 후보 모두에서 같은 `linker stdout` 줄 두 개를 출력한다. 이 변경이 아니라 벤치의 Windows release 링크에서 나오며, 이 브랜치의 ci.yml 실행에는 경고가 없다.

## 자동 측정: `main` 최신 측정 커밋

`main` 에 올라간 커밋이 CI 와 구조 성능 게이트를 모두 통과하고 직전 측정 이후 에이전트 코드(`src`, `crates`, `Cargo.toml`, `Cargo.lock`, `.cargo`, `rust-toolchain.toml`, `scripts/perf`)가 바뀌었으면, `.github/workflows/benchmark-docs.yml` 이 Linux·macOS·Windows에서 전체 벤치마크 묶음을 OS마다 따로 돌리고 아래 구간을 `main` 과 `dev` 에서 함께 다시 쓴다. OS마다 절이 하나씩 있고, 한 번의 실행이 이 파일과 `RESULTS.ko.md`를 함께 쓰므로 영어와 한국어 구간은 늘 같은 커밋을 가리킨다. `suites=all`은 모든 정규 묶음을 뜻하고 `install`은 넣지 않으므로 아래 어느 줄도 설치 시간이 아니다. 설치 시간은 설치 묶음을 이름으로 지정한 실행에서 나오고, 그 묶음은 측정 중인 커밋이 아니라 고정한 배포 릴리스를 잰다. 측정 방법과 묶음별 뜻은 [`BENCHMARK.ko.md`](BENCHMARK.ko.md) 의 "자동 측정"에 있다.

<!-- bench-auto:start commit=882db5d7a08096d4d441b674b9865a09e597886b -->

## 헤드리스 (`horon-agent`)

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `882db5d7a08096d4d441b674b9865a09e597886b`
- 측정 시각: 2026-11-24 23:57 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/45506107561> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.23-dev (882db5d)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.23-dev (882db5d)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.23-dev (882db5d)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.23-dev (882db5d) | 2.9 ms | 124.9 MB |
| jcode | jcode v1.1.2 (4f874c9ea) | 7.6 ms | 147.2 MB |
| Claude Code | 2.1.331 (Claude Code) | 7.7 ms | 760.5 MB |
| Codex CLI | codex-cli 0.172.0 | 30.0 ms | 428.5 MB |
| OpenCode | 1.19.6 | 353.4 ms | 617.5 MB |
| pi | 1.6.0 | 171.8 ms | 117.0 MB |
| oh-my-pi (omp) | omp/19.1.1 | 618.1 ms | 450.6 MB |
| oh-my-openagent (omo) | omo 5.1.83 (engine: senpi 2026.11.23) | 103.5 ms | 579.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | request bytes | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 119.1 ms | 97.8 ms–377.7 ms | baseline | 52.0 MB | baseline | yes | 10 | 3,747 / 2,358 (1 tools) | — |
| jcode | 139.5 ms | 139.0 ms–140.7 ms | 1.17× more | 51.7 MB | same | yes | 5 | 2,935 / 1,100 (1 tools) | — |
| Claude Code | 344.7 ms | 333.1 ms–346.6 ms | 2.89× more | 220.7 MB | 4.24× more | yes | 0 | 48,919 / 31,215 (12 tools) | — |
| Codex CLI | 266.7 ms | 263.0 ms–295.4 ms | 2.24× more | 187.8 MB | 3.61× more | yes | 3 | 40,520 / 18,198 (9 tools) | — |
| OpenCode | 2.18 s | 2.09 s–2.31 s | 18.33× more | 620.9 MB | 11.93× more | yes | 4 | 2,659 / 0 (0 tools) | — |
| pi | 287.3 ms | 285.5 ms–302.5 ms | 2.41× more | 120.2 MB | 2.31× more | yes | 0 | 5,502 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 1.43 s | 1.42 s–1.49 s | 12.02× more | 378.4 MB | 7.27× more | yes | 25 | 28,597 / 18,214 (11 tools) | — |
| oh-my-openagent (omo) | 4.73 s | 4.60 s–5.33 s | 39.70× more | 771.7 MB | 14.83× more | yes | 38 | 28,299 / 2,946 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 43.9 ms | 43.4 ms–44.1 ms | baseline | 43.6 MB | baseline | — | yes | — | 11,080 / 9,033 (8 tools) |
| jcode | — | — | — | — | — | — | — | skipped: jcode has no core tool profile | — |
| Claude Code | — | — | — | — | — | — | — | skipped: Claude Code has no core tool profile | — |
| Codex CLI | — | — | — | — | — | — | — | skipped: Codex CLI has no core tool profile | — |
| OpenCode | — | — | — | — | — | — | — | skipped: OpenCode has no core tool profile | — |
| pi | — | — | — | — | — | — | — | skipped: pi has no core tool profile | — |
| oh-my-pi (omp) | — | — | — | — | — | — | — | skipped: oh-my-pi (omp) has no core tool profile | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | — | skipped: oh-my-openagent (omo) has no core tool profile | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 40.6 ms | 40.5 ms–41.9 ms | baseline | 41.0 MB | baseline | skill-uri | yes | — |
| jcode | 42.2 ms | 40.3 ms–43.7 ms | 1.04× more | 49.6 MB | 1.21× more | skill-tool | yes | — |
| Claude Code | 330.1 ms | 325.9 ms–337.3 ms | 8.12× more | 214.9 MB | 5.24× more | skill-tool | yes | — |
| Codex CLI | 312.6 ms | 300.3 ms–319.7 ms | 7.69× more | 189.8 MB | 4.63× more | file-read | yes | — |
| OpenCode | 2.48 s | 2.41 s–2.49 s | 61.08× more | 662.0 MB | 16.15× more | skill-tool | yes | — |
| pi | 384.5 ms | 375.3 ms–393.9 ms | 9.46× more | 124.5 MB | 3.04× more | file-read | yes | — |
| oh-my-pi (omp) | 1.42 s | 1.41 s–1.43 s | 34.91× more | 366.4 MB | 8.94× more | file-read | yes | — |
| oh-my-openagent (omo) | 5.28 s | 5.11 s–5.38 s | 129.97× more | 753.4 MB | 18.38× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 60.4 ms | 58.3 ms–63.3 ms | baseline | 46.2 MB | baseline | mcp-call | yes | 49.5 ms | — |
| jcode | 148.8 ms | 148.4 ms–149.7 ms | 2.47× more | 59.8 MB | 1.29× more | mcp-action | yes | 137.6 ms | — |
| Claude Code | 429.4 ms | 425.2 ms–436.0 ms | 7.11× more | 239.8 MB | 5.19× more | mcp-tool | yes | 300.7 ms | — |
| Codex CLI | 287.9 ms | 278.0 ms–295.2 ms | 4.77× more | 194.4 MB | 4.21× more | mcp-tool | yes | 171.6 ms | — |
| OpenCode | 2.24 s | 2.24 s–2.27 s | 37.12× more | 604.6 MB | 13.08× more | mcp-tool | yes | 2.07 s | — |
| pi | 418.1 ms | 411.0 ms–423.7 ms | 6.93× more | 140.9 MB | 3.05× more | mcp-tool | yes | 375.2 ms | — |
| oh-my-pi (omp) | 2.33 s | 2.27 s–2.41 s | 38.67× more | 659.4 MB | 14.27× more | mcp-command | yes | 2.16 s | — |
| oh-my-openagent (omo) | 5.40 s | 5.32 s–5.42 s | 89.52× more | 735.1 MB | 15.91× more | mcp-tool | yes | 2.74 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 41.1 MB | 109.8 KB | baseline | 0.35 | 1.00 | 1 | 0.00 | 49.4 MB | 528.7 KB |
| jcode | 52.5 MB | 63.8 KB | 1.72× less | 0.20 | — | 1 | 0.00 | 55.6 MB | 187.9 KB |
| Claude Code | 164.3 MB | 164.5 MB | 1,534× more | — | — | 1 | 1.00 | 207.3 MB | 200.8 MB |
| Codex CLI | 119.9 MB | 124.9 MB | 1,164× more | — | — | 1 | 1.32 | 158.5 MB | 160.4 MB |
| OpenCode | 242.6 MB | 0.0 B | — | — | — | 1 | 0.00 | 462.1 MB | 11.2 MB |
| pi | 114.8 MB | 115.0 MB | 1,072× more | — | — | 1 | 1.00 | 130.8 MB | 132.9 MB |
| oh-my-pi (omp) | 379.7 MB | 102.3 MB | 954.15× more | — | — | 1 | 1.00 | 386.5 MB | 408.6 MB |
| oh-my-openagent (omo) | 430.7 MB | 37.6 MB | 350.62× more | — | — | 2 | 0.89 | 671.9 MB | 529.0 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 49.4 MB | 52.5 MB | 54.2 MB |
| jcode | 55.6 MB | 56.5 MB | 57.3 MB |
| Claude Code | 207.3 MB | 1021.9 MB | 2.0 GB |
| Codex CLI | 158.5 MB | 793.6 MB | 1.6 GB |
| OpenCode | 462.1 MB | 586.2 MB | 568.7 MB |
| pi | 130.8 MB | 651.4 MB | 1.3 GB |
| oh-my-pi (omp) | 386.5 MB | 1.9 GB | 4.0 GB |
| oh-my-openagent (omo) | 671.9 MB | 3.0 GB | 5.3 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 1 / 1 | 5 / 5 | 10 / 10 |
| Codex CLI | 1 / 1 | 8 / 5 | 13 / 10 |
| OpenCode | 1 / 1 | 1 / 1 | 1 / 1 |
| pi | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-pi (omp) | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-openagent (omo) | 2 / 4 | 5 / 20 | 10 / 40 |

#### User-installed package cost (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. One turn per sample; samples per scenario: 5. Cells are measured per OS; never compare absolute values across OSes.

| Build | Scenario | Turn (median) | Min–max | First request | Peak tree RSS | Tool round trip | Tools called | vs baseline | Status |
|---|---|---|---|---|---|---|---|---|---|
| candidate | empty | 28.4 ms | 27.9 ms–122.9 ms | 24.8 ms | 30.6 MB | no | – | – | ok |
| candidate | installed | 34.1 ms | 28.9 ms–35.3 ms | 30.0 ms | 40.0 MB | no | – | – | ok |
| candidate | called | 60.2 ms | 59.8 ms–65.1 ms | 25.4 ms | 47.1 MB | yes | ext__hello__greet | – | ok |

**Installed size** (absolute: a size does not need a median)

| Build | Scenario | Package area | Files | Package binary | Agent binary |
|---|---|---|---|---|---|
| candidate | empty | 0.0 B | 0 | 1.6 MB | 124.9 MB |
| candidate | installed | 1.6 MB | 4 | 1.6 MB | 124.9 MB |
| candidate | called | 1.6 MB | 4 | 1.6 MB | 124.9 MB |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    3.80 ms
  Max:    4.19 ms
  Mean:   3.92 ms
  Median: 3.91 ms
  Stdev:  0.11 ms

--version:
  Min:    3.60 ms
  Max:    3.74 ms
  Mean:   3.66 ms
  Median: 3.66 ms
  Stdev:  0.04 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.46 ms
  Max:    20.76 ms
  Mean:   20.55 ms
  Median: 20.52 ms
  Stdev:  0.09 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        0.2       0.2   19.4%
  nofile_limit        0.4       0.3   29.8%
  perm_harden         0.4       0.0    1.4%
  args_parse          0.4       0.0    0.1%
  config_loaded       0.6       0.1   15.3%
  server_new          0.7       0.2   22.1%
  socket_bound        0.8       0.1   11.8%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.57 ms
  Max:    20.66 ms
  Mean:   20.63 ms
  Median: 20.63 ms
  Stdev:  0.03 ms

Bridge stage: hello_ok:
  Min:    21.13 ms
  Max:    21.36 ms
  Mean:   21.28 ms
  Median: 21.32 ms
  Stdev:  0.09 ms

Bridge stage: no-op reply:
  Min:    21.49 ms
  Max:    21.77 ms
  Mean:   21.64 ms
  Median: 21.68 ms
  Stdev:  0.10 ms

Bridge total (socket -> no-op):
  Min:    21.49 ms
  Max:    21.77 ms
  Mean:   21.64 ms
  Median: 21.68 ms
  Stdev:  0.10 ms

============================================================
Summary:
  Binary load median:      ~3.9 ms
  Server ready median:     ~20.5 ms
  Bridge total median:     ~21.7 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (6c729a5d90a4)
stages   /home/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (b66c688ac1b3, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-4m5duch8
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.007/0.007    p95    0.007/0.008    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.041/0.041    p95    0.045/0.041    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.053    p95    0.067/0.059    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    4.391/4.381    p95    6.470/4.580    delta  -0.010 ms  control-no-change
  total store_read_bytes p50 23080  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23072..23092 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23088 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    1.209/1.195    p95    1.220/1.351    delta  -0.014 ms  control-no-change
  skill_request_overlay p50    1.811/1.847    p95    1.888/1.901    delta  +0.036 ms  control-no-change
  mcp_config_load   p50    0.057/0.057    p95    0.059/0.068    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    6.168/6.229    p95    7.043/7.406    delta  +0.061 ms  control-no-change
  total store_read_bytes p50 23076  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23068..23088 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23084 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.006/0.006    p95    0.007/0.007    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.041/0.041    p95    0.058/0.042    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.053    p95    0.055/0.063    delta  -0.000 ms  control-no-change
  attach_history    p50  151.187/151.948  p95  152.161/154.464  delta  +0.761 ms  control-no-change
  resume_ready      p50   27.066/27.129   p95   27.320/27.397   delta  +0.063 ms  control-no-change
  provider_dispatch p50    0.992/0.988    p95    1.004/1.001    delta  -0.004 ms  control-no-change
  session_save      p50    4.574/4.559    p95    4.771/5.047    delta  -0.015 ms  control-no-change
  total store_read_bytes p50 5311405  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 151.187 ms (load 124.155 + history 26.983)
    selection          p50     0.019 ms  min     0.016  max     0.023  ratio 0.0% of anchor
    checkpoint_recent  p50    96.978 ms  min    96.737  max    98.211  ratio 64.1% of anchor
    log_read           p50     0.052 ms  min     0.050  max     0.053  ratio 0.0% of anchor
    full_scan          p50    15.032 ms  min    14.980  max    15.085  ratio 9.9% of anchor
    blob_read          p50     0.004 ms  min     0.004  max     0.004  ratio 0.0% of anchor
    graph_replay       p50    10.562 ms  min    10.553  max    10.735  ratio 7.0% of anchor
    metadata_json      p50     0.006 ms  min     0.006  max     0.007  ratio 0.0% of anchor
    messages_json      p50     0.708 ms  min     0.708  max     0.716  ratio 0.5% of anchor
    messages_decode    p50     0.672 ms  min     0.664  max     0.679  ratio 0.4% of anchor
    source_hash        p50     0.372 ms  min     0.369  max     0.388  ratio 0.2% of anchor
    history_rescan     p50    29.128 ms  min    28.977  max    29.184  ratio 19.3% of anchor
    page_index         p50    31.520 ms  min    31.427  max    31.647  ratio 20.8% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099130..1099134 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23062..23092 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189183..4189191 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099126..1099134 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23080 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.187/1.196    p95    1.219/1.209    delta  +0.010 ms  control-no-change
  skill_request_overlay p50    1.833/1.829    p95    1.850/1.874    delta  -0.004 ms  control-no-change
  mcp_config_load   p50    0.057/0.057    p95    0.064/0.063    delta  +0.000 ms  control-no-change
  attach_history    p50  152.433/152.559  p95  153.827/153.525  delta  +0.125 ms  control-no-change
  resume_ready      p50   27.108/27.179   p95   27.431/27.265   delta  +0.071 ms  control-no-change
  provider_dispatch p50    0.988/0.991    p95    1.016/1.021    delta  +0.004 ms  control-no-change
  session_save      p50    4.485/4.463    p95    4.964/4.750    delta  -0.023 ms  control-no-change
  total store_read_bytes p50 5311406  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 152.433 ms (load 125.290 + history 27.082)
    selection          p50     0.018 ms  min     0.017  max     0.021  ratio 0.0% of anchor
    checkpoint_recent  p50    97.186 ms  min    96.155  max    97.469  ratio 63.8% of anchor
    log_read           p50     0.050 ms  min     0.048  max     0.072  ratio 0.0% of anchor
    full_scan          p50    14.879 ms  min    14.846  max    14.995  ratio 9.8% of anchor
    blob_read          p50     0.004 ms  min     0.003  max     0.004  ratio 0.0% of anchor
    graph_replay       p50    10.506 ms  min    10.474  max    10.522  ratio 6.9% of anchor
    metadata_json      p50     0.006 ms  min     0.006  max     0.006  ratio 0.0% of anchor
    messages_json      p50     0.714 ms  min     0.710  max     0.721  ratio 0.5% of anchor
    messages_decode    p50     0.656 ms  min     0.642  max     0.687  ratio 0.4% of anchor
    source_hash        p50     0.383 ms  min     0.363  max     0.385  ratio 0.3% of anchor
    history_rescan     p50    28.968 ms  min    28.700  max    29.064  ratio 19.0% of anchor
    page_index         p50    30.941 ms  min    30.735  max    31.045  ratio 20.3% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189195 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099128..1099136 (4 distinct values)
  SPREAD   a session_save store_read_bytes 23065..23086 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189193..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099135..1099136 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23072..23094 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.006/0.006    p95    0.009/0.007    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.040/0.041    p95    0.042/0.049    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.053    p95    0.065/0.055    delta  -0.000 ms  control-no-change
  attach_history    p50 1893.504/1889.946 p95 1923.222/1921.325 delta  -3.559 ms  control-no-change
  resume_ready      p50  320.625/321.219  p95  326.058/330.877  delta  +0.594 ms  control-no-change
  provider_dispatch p50   11.604/11.581   p95   11.931/11.959   delta  -0.023 ms  control-no-change
  session_save      p50    4.724/4.828    p95    5.029/5.228    delta  +0.104 ms  control-no-change
  total store_read_bytes p50 53417626  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 1893.504 ms (load 1561.297 + history 329.812)
    selection          p50     0.024 ms  min     0.022  max     0.025  ratio 0.0% of anchor
    checkpoint_recent  p50  1197.745 ms  min  1196.427  max  1228.450  ratio 63.3% of anchor
    log_read           p50     0.741 ms  min     0.723  max     0.783  ratio 0.0% of anchor
    full_scan          p50   165.598 ms  min   164.816  max   166.979  ratio 8.7% of anchor
    blob_read          p50     0.042 ms  min     0.029  max     0.048  ratio 0.0% of anchor
    graph_replay       p50   139.648 ms  min   138.578  max   146.459  ratio 7.4% of anchor
    metadata_json      p50     0.005 ms  min     0.005  max     0.006  ratio 0.0% of anchor
    messages_json      p50     6.507 ms  min     6.346  max     6.566  ratio 0.3% of anchor
    messages_decode    p50     9.616 ms  min     9.582  max     9.973  ratio 0.5% of anchor
    source_hash        p50     3.911 ms  min     3.885  max     3.963  ratio 0.2% of anchor
    history_rescan     p50   342.591 ms  min   337.681  max   349.162  ratio 18.1% of anchor
    page_index         p50   353.611 ms  min   348.537  max   355.775  ratio 18.7% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23077..23097 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23069..23093 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.179/1.195    p95    1.214/1.221    delta  +0.016 ms  control-no-change
  skill_request_overlay p50    1.779/1.771    p95    1.824/1.819    delta  -0.008 ms  control-no-change
  mcp_config_load   p50    0.057/0.058    p95    0.058/0.075    delta  +0.000 ms  control-no-change
  attach_history    p50 1881.789/1868.507 p95 1925.234/1899.835 delta -13.281 ms  control-no-change
  resume_ready      p50  320.062/319.930  p95  325.949/325.232  delta  -0.132 ms  control-no-change
  provider_dispatch p50   11.601/11.624   p95   12.016/11.850   delta  +0.023 ms  control-no-change
  session_save      p50    4.694/4.810    p95    5.046/4.957    delta  +0.116 ms  control-no-change
  total store_read_bytes p50 53417622  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 1881.789 ms (load 1552.305 + history 323.466)
    selection          p50     0.024 ms  min     0.023  max     0.026  ratio 0.0% of anchor
    checkpoint_recent  p50  1239.645 ms  min  1229.672  max  1240.514  ratio 65.9% of anchor
    log_read           p50     0.825 ms  min     0.808  max     0.858  ratio 0.0% of anchor
    full_scan          p50   164.753 ms  min   164.246  max   165.678  ratio 8.8% of anchor
    blob_read          p50     0.078 ms  min     0.065  max     0.079  ratio 0.0% of anchor
    graph_replay       p50   152.086 ms  min   150.697  max   152.451  ratio 8.1% of anchor
    metadata_json      p50     0.006 ms  min     0.005  max     0.006  ratio 0.0% of anchor
    messages_json      p50     6.535 ms  min     6.424  max     6.643  ratio 0.3% of anchor
    messages_decode    p50    10.296 ms  min    10.100  max    10.614  ratio 0.5% of anchor
    source_hash        p50     3.971 ms  min     3.943  max     3.974  ratio 0.2% of anchor
    history_rescan     p50   352.471 ms  min   352.382  max   357.273  ratio 18.7% of anchor
    page_index         p50   364.279 ms  min   364.189  max   366.477  ratio 19.4% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184287..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210241..11210246 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23071..23097 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184291..42184293 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210245..11210246 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23053..23089 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (6c729a5d90a4)
stages   /home/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (b66c688ac1b3, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-9zft_e2k
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.016/0.016    p95    0.035/0.020    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.110/0.109    p95    0.138/0.110    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.144/0.143    p95    0.147/0.167    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    8.493/8.805    p95    9.634/10.010   delta  +0.311 ms  control-no-change
  total store_read_bytes p50 23776  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23065, 23069, 23077, 23085, 23765, 23787, 23791, 24497, 24505, 24515]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23073, 23077, 23783, 23787, 23791, 24489, 24501, 24513, 24517]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23065..24515 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23073..24517 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.880/2.889    p95    2.924/2.967    delta  +0.008 ms  control-no-change
  skill_request_overlay p50    5.394/5.323    p95    5.625/5.466    delta  -0.071 ms  control-no-change
  mcp_config_load   p50    0.151/0.152    p95    0.183/0.159    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   10.295/10.100   p95   11.537/12.050   delta  -0.195 ms  control-no-change
  total store_read_bytes p50 23790  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23061, 23073, 23075, 23787, 23789, 23791, 24495, 24503, 24509, 24513]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23071, 23073, 23077, 23793, 23795, 24497, 24501, 24503]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23061..24513 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23071..24503 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.016/0.015    p95    0.021/0.017    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.109/0.109    p95    0.134/0.109    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.142/0.143    p95    0.162/0.178    delta  +0.001 ms  control-no-change
  attach_history    p50  104.616/103.668  p95  106.956/106.174  delta  -0.948 ms  control-no-change
  resume_ready      p50   55.006/55.417   p95   56.896/56.283   delta  +0.411 ms  control-no-change
  provider_dispatch p50    1.439/1.449    p95    1.491/1.467    delta  +0.010 ms  control-no-change
  session_save      p50    8.532/8.295    p95   10.671/9.527    delta  -0.237 ms  control-no-change
  total store_read_bytes p50 2841363  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23789, 23799, 24499, 24513, 25231, 25949, 25953, 26657, 26671]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23787, 24509, 25223, 25225, 25935, 25941, 26651, 26665]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23789..26671 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23787..26665 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.914/2.878    p95    3.029/2.943    delta  -0.036 ms  control-no-change
  skill_request_overlay p50    4.961/4.897    p95    5.795/5.502    delta  -0.063 ms  control-no-change
  mcp_config_load   p50    0.151/0.152    p95    0.160/0.165    delta  +0.001 ms  control-no-change
  attach_history    p50  104.958/103.595  p95  106.107/106.937  delta  -1.362 ms  control-no-change
  resume_ready      p50   55.318/54.952   p95   55.878/56.521   delta  -0.366 ms  control-no-change
  provider_dispatch p50    1.430/1.436    p95    1.453/1.486    delta  +0.007 ms  control-no-change
  session_save      p50    9.427/9.343    p95   10.179/10.640   delta  -0.084 ms  control-no-change
  total store_read_bytes p50 2841356  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23785, 23795, 24499, 25221, 25227, 25945, 25959, 26663, 26671]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23791, 23803, 24509, 24517, 25203, 25229, 25937, 25951, 26657, 26663]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23785..26671 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23791..26663 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.016    p95    0.020/0.018    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.109/0.109    p95    0.153/0.136    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.143    p95    0.206/0.166    delta  +0.000 ms  control-no-change
  attach_history    p50 1180.456/1168.431 p95 1212.650/1196.882 delta -12.025 ms  control-no-change
  resume_ready      p50  676.030/673.824  p95  692.061/687.238  delta  -2.205 ms  control-no-change
  provider_dispatch p50   19.346/19.650   p95   20.056/21.159   delta  +0.305 ms  control-no-change
  session_save      p50    8.389/8.428    p95    9.843/8.920    delta  +0.039 ms  control-no-change
  total store_read_bytes p50 28640378  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23781, 23795, 24511, 24521, 25223, 25231, 25941, 25953, 26657, 26663]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23791, 23795, 24509, 24513, 25227, 25231, 25933, 25941, 26667, 26671]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23781..26663 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23791..26671 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.882/2.895    p95    2.946/2.956    delta  +0.014 ms  control-no-change
  skill_request_overlay p50    4.838/4.874    p95    4.903/5.137    delta  +0.037 ms  control-no-change
  mcp_config_load   p50    0.151/0.151    p95    0.158/0.153    delta  -0.000 ms  control-no-change
  attach_history    p50 1170.906/1177.793 p95 1177.071/1189.191 delta  +6.887 ms  control-no-change
  resume_ready      p50  678.277/679.585  p95  689.156/692.139  delta  +1.307 ms  control-no-change
  provider_dispatch p50   19.004/18.799   p95   19.934/20.049   delta  -0.205 ms  control-no-change
  session_save      p50    9.875/9.700    p95   10.636/10.820   delta  -0.176 ms  control-no-change
  total store_read_bytes p50 28640386  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23795, 23797, 24497, 24503, 25231, 25239, 25947, 25957, 26659, 26673]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23803, 24509, 24525, 25231, 25239, 25939, 25965, 26665, 26679]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23795..26673 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23803..26679 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |
| read (1000 lines) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (10 lines @ offset) | 0.2 ms | 0.2 ms | 0.2 ms | 0.2 ms | 5 |
| read (repo Cargo.toml) | 0.2 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |
| write (small) | 0.6 ms | 0.7 ms | 0.6 ms | 0.7 ms | 5 |
| write (10KB) | 0.7 ms | 0.8 ms | 0.7 ms | 0.8 ms | 5 |
| edit (simple replace) | 0.6 ms | 0.6 ms | 0.6 ms | 0.6 ms | 5 |
| ls (scripts dir) | 0.5 ms | 0.6 ms | 0.5 ms | 0.6 ms | 5 |
| agentgrep (grep in tree) | 1.2 ms | 1.3 ms | 1.1 ms | 1.3 ms | 5 |
| agentgrep (find **/*.rs) | 0.9 ms | 1.0 ms | 0.9 ms | 1.0 ms | 5 |
| bash (true) | 1.4 ms | 1.6 ms | 1.3 ms | 1.6 ms | 5 |
| bash (echo hello) | 1.3 ms | 1.4 ms | 1.2 ms | 1.4 ms | 5 |
| bash (ls fixture tree) | 2.3 ms | 2.3 ms | 2.3 ms | 2.3 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.0 ms | 0.1 ms | 5 |

#### Tool CPU per call

| Tool case | daemon cpu median | min | max | child cpu median | voluntary switches | involuntary switches | windows × calls | source |
|---|---|---|---|---|---|---|---|---|
| read (tiny file) | 0.111 ms | 0.109 ms | 0.126 ms | 0.000 ms | 4.18 | 0.00 | 5 × 50 | schedstat |
| read (1000 lines) | 0.213 ms | 0.209 ms | 0.224 ms | 0.000 ms | 5.26 | 0.00 | 5 × 50 | schedstat |
| read (10 lines @ offset) | 0.213 ms | 0.206 ms | 0.216 ms | 0.000 ms | 5.24 | 0.00 | 5 × 50 | schedstat |
| read (repo Cargo.toml) | 0.162 ms | 0.159 ms | 0.164 ms | 0.000 ms | 5.30 | 0.00 | 5 × 50 | schedstat |
| write (small) | 0.408 ms | 0.375 ms | 0.427 ms | 0.000 ms | 16.90 | 0.00 | 5 × 50 | schedstat |
| write (10KB) | 0.542 ms | 0.496 ms | 0.579 ms | 0.000 ms | 17.74 | 0.02 | 5 × 50 | schedstat |
| edit (simple replace) | 0.459 ms | 0.418 ms | 0.513 ms | 0.000 ms | 17.08 | 0.00 | 5 × 50 | schedstat |
| ls (scripts dir) | 0.458 ms | 0.443 ms | 0.472 ms | 0.000 ms | 5.52 | 0.00 | 5 × 50 | schedstat |
| agentgrep (grep in tree) | 0.866 ms | 0.858 ms | 0.886 ms | 0.000 ms | 9.62 | 0.08 | 5 × 50 | schedstat |
| agentgrep (find **/*.rs) | 0.895 ms | 0.825 ms | 0.901 ms | 0.000 ms | 5.50 | 0.00 | 5 × 50 | schedstat |
| bash (true) | 0.422 ms | 0.419 ms | 0.429 ms | 1.000 ms | 7.82 | 0.00 | 5 × 50 | schedstat |
| bash (echo hello) | 0.448 ms | 0.430 ms | 0.455 ms | 1.000 ms | 8.38 | 0.04 | 5 × 50 | schedstat |
| bash (ls fixture tree) | 0.454 ms | 0.448 ms | 0.463 ms | 1.600 ms | 8.16 | 0.04 | 5 × 50 | schedstat |
| invalid tool (dispatch only) | 0.025 ms | 0.023 ms | 0.027 ms | 0.000 ms | 2.76 | 0.00 | 5 × 50 | schedstat |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 38.5 ms | 38.5 ms | 38.5 ms | 38.8 ms | 0 |
| 10 | 131.8 ms | 175.3 ms | 175.3 ms | 175.5 ms | 0 |
| 40 | 524.0 ms | 595.7 ms | 607.3 ms | 616.1 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 212.5 ms | 212.5 ms | 212.5 ms | 1 |
| leaf_touch | src/main.rs | 258.6 ms | 258.6 ms | 258.6 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 18.96 s | 18.96 s | 18.96 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 26.58 s | 26.58 s | 26.58 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.23-dev (882db5d) | 16.0 ms | 105.0 MB |
| jcode | jcode v1.0.3 (a0ae514c4) | 15.0 ms | 154.9 MB |
| Claude Code | 2.1.331 (Claude Code) | 38.4 ms | 727.8 MB |
| Codex CLI | codex-cli 0.172.0 | 114.2 ms | 382.0 MB |
| OpenCode | 1.19.6 | 624.4 ms | 347.4 MB |
| pi | 1.6.0 | 299.1 ms | 117.0 MB |
| oh-my-pi (omp) | omp/19.1.1 | 876.0 ms | 295.8 MB |
| oh-my-openagent (omo) | omo 5.1.83 (engine: senpi 2026.11.23) | 260.4 ms | 560.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 159.3 ms | 132.1 ms–652.9 ms | baseline | 33.3 MB | baseline | yes | 3,588 / 2,358 (1 tools) | — |
| jcode | 436.9 ms | 392.2 ms–890.1 ms | 2.74× more | 36.1 MB | 1.08× more | yes | 2,776 / 1,100 (1 tools) | — |
| Claude Code | 610.8 ms | 589.6 ms–1.47 s | 3.83× more | 216.0 MB | 6.48× more | yes | 49,411 / 31,689 (12 tools) | — |
| Codex CLI | 591.8 ms | 394.2 ms–2.20 s | 3.72× more | 141.9 MB | 4.26× more | yes | 40,541 / 18,198 (9 tools) | — |
| OpenCode | 5.21 s | 3.78 s–7.36 s | 32.74× more | 799.9 MB | 23.99× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 554.2 ms | 485.9 ms–622.1 ms | 3.48× more | 128.1 MB | 3.84× more | yes | 5,513 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.41 s | 2.22 s–2.92 s | 15.14× more | 434.5 MB | 13.03× more | yes | 27,574 / 18,214 (11 tools) | — |
| oh-my-openagent (omo) | 13.76 s | 8.50 s–15.23 s | 86.39× more | 948.2 MB | 28.44× more | yes | 28,349 / 2,946 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 139.1 ms | 126.6 ms–191.1 ms | baseline | 26.7 MB | baseline | — | yes | — | 10,921 / 9,033 (8 tools) |
| jcode | — | — | — | — | — | — | — | skipped: jcode has no core tool profile | — |
| Claude Code | — | — | — | — | — | — | — | skipped: Claude Code has no core tool profile | — |
| Codex CLI | — | — | — | — | — | — | — | skipped: Codex CLI has no core tool profile | — |
| OpenCode | — | — | — | — | — | — | — | skipped: OpenCode has no core tool profile | — |
| pi | — | — | — | — | — | — | — | skipped: pi has no core tool profile | — |
| oh-my-pi (omp) | — | — | — | — | — | — | — | skipped: oh-my-pi (omp) has no core tool profile | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | — | skipped: oh-my-openagent (omo) has no core tool profile | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 137.5 ms | 92.2 ms–164.6 ms | baseline | 31.2 MB | baseline | skill-uri | yes | — |
| jcode | 323.6 ms | 298.3 ms–395.4 ms | 2.35× more | 38.9 MB | 1.25× more | skill-tool | yes | — |
| Claude Code | 878.8 ms | 741.1 ms–913.9 ms | 6.39× more | 213.9 MB | 6.86× more | skill-tool | yes | — |
| Codex CLI | 450.0 ms | 423.6 ms–563.6 ms | 3.27× more | 143.4 MB | 4.60× more | file-read | yes | — |
| OpenCode | 5.49 s | 4.79 s–5.77 s | 39.92× more | 792.8 MB | 25.43× more | skill-tool | yes | — |
| pi | 743.8 ms | 675.7 ms–975.3 ms | 5.41× more | 132.5 MB | 4.25× more | file-read | yes | — |
| oh-my-pi (omp) | 2.24 s | 1.88 s–2.76 s | 16.28× more | 388.3 MB | 12.46× more | file-read | yes | — |
| oh-my-openagent (omo) | 11.01 s | 10.58 s–11.62 s | 80.10× more | 933.6 MB | 29.95× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 141.9 ms | 120.0 ms–191.7 ms | baseline | 42.3 MB | baseline | mcp-call | yes | 107.5 ms | — |
| jcode | 364.8 ms | 344.6 ms–496.9 ms | 2.57× more | 54.2 MB | 1.28× more | mcp-action | yes | 340.3 ms | — |
| Claude Code | 1.01 s | 726.4 ms–1.48 s | 7.11× more | 237.0 MB | 5.60× more | mcp-tool | yes | 784.6 ms | — |
| Codex CLI | 393.9 ms | 353.9 ms–452.9 ms | 2.78× more | 140.8 MB | 3.33× more | mcp-tool | yes | 267.9 ms | — |
| OpenCode | 4.81 s | 4.29 s–5.14 s | 33.88× more | 720.0 MB | 17.02× more | mcp-tool | yes | 4.17 s | — |
| pi | 754.4 ms | 695.4 ms–780.8 ms | 5.32× more | 133.9 MB | 3.16× more | mcp-tool | yes | 663.3 ms | — |
| oh-my-pi (omp) | 4.65 s | 3.76 s–5.99 s | 32.80× more | 953.7 MB | 22.54× more | mcp-command | yes | 4.17 s | — |
| oh-my-openagent (omo) | 11.39 s | 11.32 s–13.92 s | 80.28× more | 955.5 MB | 22.58× more | mcp-tool | yes | 6.98 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 31.1 MB | 141.1 KB | baseline | 0.45 | 1.00 | 1 | 0.00 | 37.7 MB | 467.7 KB |
| jcode | 38.3 MB | 171.9 KB | 1.22× more | — | — | 1 | 0.00 | 41.7 MB | 310.6 KB |
| Claude Code | 171.2 MB | 157.8 MB | 1,145× more | — | — | 1 | 1.00 | 205.5 MB | 201.8 MB |
| Codex CLI | 98.5 MB | 77.3 MB | 560.84× more | — | — | 2 | 0.89 | 108.9 MB | 109.3 MB |
| OpenCode | 262.2 MB | 36.3 MB | 263.32× more | — | — | 1 | 0.00 | 569.8 MB | 46.8 MB |
| pi | 120.8 MB | 118.8 MB | 862.10× more | — | — | 1 | 1.00 | 149.5 MB | 150.0 MB |
| oh-my-pi (omp) | 356.9 MB | 143.3 MB | 1,040× more | — | — | 1 | 1.00 | 498.6 MB | 356.1 MB |
| oh-my-openagent (omo) | 62.9 MB | 50.0 MB | 362.60× more | — | — | 1 | 1.00 | 898.2 MB | 364.0 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 37.7 MB | 39.3 MB | 41.8 MB |
| jcode | 41.7 MB | 42.7 MB | 44.4 MB |
| Claude Code | 205.5 MB | 1020.8 MB | 2.0 GB |
| Codex CLI | 108.9 MB | 547.8 MB | 1.1 GB |
| OpenCode | 569.8 MB | 648.8 MB | 983.4 MB |
| pi | 149.5 MB | 745.2 MB | 1.5 GB |
| oh-my-pi (omp) | 498.6 MB | 2.0 GB | 3.6 GB |
| oh-my-openagent (omo) | 898.2 MB | 3.0 GB | 4.1 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 1 / 1 | 5 / 5 | 10 / 10 |
| Codex CLI | 2 / 1 | 5 / 5 | 10 / 10 |
| OpenCode | 1 / 1 | 1 / 1 | 1 / 1 |
| pi | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-pi (omp) | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-openagent (omo) | 1 / 4 | 5 / 20 | 10 / 40 |

#### User-installed package cost (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. One turn per sample; samples per scenario: 5. Cells are measured per OS; never compare absolute values across OSes.

| Build | Scenario | Turn (median) | Min–max | First request | Peak tree RSS | Tool round trip | Tools called | vs baseline | Status |
|---|---|---|---|---|---|---|---|---|---|
| candidate | empty | 58.5 ms | 51.9 ms–280.9 ms | 43.2 ms | 29.0 MB | no | – | – | ok |
| candidate | installed | 78.2 ms | 59.3 ms–123.5 ms | 63.8 ms | 27.2 MB | no | – | – | ok |
| candidate | called | 139.3 ms | 120.1 ms–186.8 ms | 51.8 ms | 34.4 MB | yes | ext__hello__greet | – | ok |

**Installed size** (absolute: a size does not need a median)

| Build | Scenario | Package area | Files | Package binary | Agent binary |
|---|---|---|---|---|---|
| candidate | empty | 0.0 B | 0 | 1.4 MB | 105.0 MB |
| candidate | installed | 1.4 MB | 4 | 1.4 MB | 105.0 MB |
| candidate | called | 1.4 MB | 4 | 1.4 MB | 105.0 MB |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    11.75 ms
  Max:    30.73 ms
  Mean:   18.84 ms
  Median: 18.34 ms
  Stdev:  5.39 ms

--version:
  Min:    9.82 ms
  Max:    23.92 ms
  Mean:   16.32 ms
  Median: 12.76 ms
  Stdev:  6.18 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    24.13 ms
  Max:    106.06 ms
  Mean:   69.40 ms
  Median: 71.65 ms
  Stdev:  29.04 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.0%
  logging_init        1.1       1.1   14.9%
  nofile_limit        1.4       0.3    4.2%
  perm_harden         1.5       0.0    0.6%
  args_parse          1.5       0.0    0.0%
  config_loaded       7.0       5.5   73.1%
  server_new          7.2       0.2    3.0%
  socket_bound        7.5       0.3    4.2%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    41.63 ms
  Max:    109.49 ms
  Mean:   86.67 ms
  Median: 94.25 ms
  Stdev:  21.22 ms

Bridge stage: hello_ok:
  Min:    42.65 ms
  Max:    110.88 ms
  Mean:   88.05 ms
  Median: 95.38 ms
  Stdev:  21.48 ms

Bridge stage: no-op reply:
  Min:    43.24 ms
  Max:    111.98 ms
  Mean:   89.00 ms
  Median: 95.88 ms
  Stdev:  21.76 ms

Bridge total (socket -> no-op):
  Min:    43.24 ms
  Max:    111.98 ms
  Mean:   89.00 ms
  Median: 95.88 ms
  Stdev:  21.76 ms

============================================================
Summary:
  Binary load median:      ~18.3 ms
  Server ready median:     ~71.7 ms
  Bridge total median:     ~95.9 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (aab096e2fe7c)
stages   /Users/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (3d1557c233c9, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/q2/p1t6rgxj5fj32g1dc7dmd49r0000gn/T/horon-loading-bench-_ok7old2
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.030/0.029    p95    0.045/0.046    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.242/0.236    p95    0.268/0.275    delta  -0.006 ms  control-no-change
  mcp_config_load   p50    0.282/0.274    p95    0.392/0.587    delta  -0.008 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   16.525/17.868   p95   29.419/23.113   delta  +1.342 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22938..22958 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22958 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.225/4.441    p95    5.487/5.096    delta  +0.217 ms  control-no-change
  skill_request_overlay p50    5.421/5.229    p95    8.166/7.405    delta  -0.192 ms  control-no-change
  mcp_config_load   p50    0.265/0.273    p95    0.341/0.341    delta  +0.007 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   19.094/16.859   p95   32.015/28.913   delta  -2.236 ms  control-no-change
  total store_read_bytes p50 22948  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22958 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22958 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.031/0.033    p95    0.083/0.073    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.230/0.226    p95    0.559/0.531    delta  -0.004 ms  control-no-change
  mcp_config_load   p50    0.265/0.258    p95    0.490/0.653    delta  -0.006 ms  control-no-change
  attach_history    p50  244.051/240.029  p95  335.630/312.297  delta  -4.022 ms  control-no-change
  resume_ready      p50   65.101/64.553   p95   81.836/81.558   delta  -0.548 ms  control-no-change
  provider_dispatch p50    1.135/1.072    p95    1.970/2.219    delta  -0.064 ms  control-no-change
  session_save      p50   16.908/12.435   p95  184.083/76.528   delta  -4.473 ms  control-no-change
  total store_read_bytes p50 5311312  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 244.051 ms (load 211.267 + history 34.786)
    selection          p50     0.067 ms  min     0.043  max     0.170  ratio 0.0% of anchor
    checkpoint_recent  p50   163.608 ms  min   156.655  max   163.718  ratio 67.0% of anchor
    log_read           p50     0.072 ms  min     0.052  max     0.090  ratio 0.0% of anchor
    full_scan          p50    22.125 ms  min    21.999  max    28.362  ratio 9.1% of anchor
    blob_read          p50     0.006 ms  min     0.005  max     0.014  ratio 0.0% of anchor
    graph_replay       p50    14.954 ms  min    14.571  max    23.058  ratio 6.1% of anchor
    metadata_json      p50     0.028 ms  min     0.010  max     0.058  ratio 0.0% of anchor
    messages_json      p50     0.741 ms  min     0.570  max     2.126  ratio 0.3% of anchor
    messages_decode    p50     0.583 ms  min     0.519  max     1.539  ratio 0.2% of anchor
    source_hash        p50     2.328 ms  min     2.101  max     3.562  ratio 1.0% of anchor
    history_rescan     p50    48.273 ms  min    42.079  max    60.566  ratio 19.8% of anchor
    page_index         p50    52.561 ms  min    44.503  max    57.321  ratio 21.5% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099143..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22948..22964 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22940..22968 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    3.965/4.339    p95    5.388/5.463    delta  +0.374 ms  control-no-change
  skill_request_overlay p50    5.326/5.886    p95    7.586/7.348    delta  +0.560 ms  control-no-change
  mcp_config_load   p50    0.276/0.327    p95    0.337/0.360    delta  +0.052 ms  control-no-change
  attach_history    p50  239.497/263.608  p95  326.405/334.288  delta +24.111 ms  control-no-change
  resume_ready      p50   59.915/68.755   p95   95.663/96.156   delta  +8.840 ms  control-no-change
  provider_dispatch p50    1.084/1.264    p95    1.602/2.237    delta  +0.180 ms  control-no-change
  session_save      p50   16.926/19.630   p95   27.467/44.907   delta  +2.703 ms  control-no-change
  total store_read_bytes p50 5311307  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 239.497 ms (load 202.249 + history 33.826)
    selection          p50     0.038 ms  min     0.035  max     0.101  ratio 0.0% of anchor
    checkpoint_recent  p50   147.001 ms  min   140.688  max   159.253  ratio 61.4% of anchor
    log_read           p50     0.053 ms  min     0.042  max     0.079  ratio 0.0% of anchor
    full_scan          p50    18.972 ms  min    18.775  max    19.682  ratio 7.9% of anchor
    blob_read          p50     0.003 ms  min     0.003  max     0.003  ratio 0.0% of anchor
    graph_replay       p50    12.748 ms  min    12.709  max    12.830  ratio 5.3% of anchor
    metadata_json      p50     0.008 ms  min     0.008  max     0.024  ratio 0.0% of anchor
    messages_json      p50     0.637 ms  min     0.595  max     0.734  ratio 0.3% of anchor
    messages_decode    p50     0.537 ms  min     0.497  max     0.558  ratio 0.2% of anchor
    source_hash        p50     1.930 ms  min     1.886  max     1.966  ratio 0.8% of anchor
    history_rescan     p50    35.608 ms  min    35.385  max    38.654  ratio 14.9% of anchor
    page_index         p50    45.689 ms  min    44.611  max    53.324  ratio 19.1% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189201..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099139..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22932..22960 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22959 (5 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.029/0.027    p95    0.036/0.054    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.226/0.227    p95    0.352/0.497    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.266/0.276    p95    0.723/0.783    delta  +0.010 ms  control-no-change
  attach_history    p50 2939.950/3045.650 p95 3240.469/4384.510 delta +105.701 ms  control-no-change
  resume_ready      p50  712.766/760.493  p95  862.083/995.872  delta +47.727 ms  control-no-change
  provider_dispatch p50   11.538/13.098   p95   15.246/18.194   delta  +1.559 ms  control-no-change
  session_save      p50   22.758/20.553   p95   64.643/50.095   delta  -2.205 ms  control-no-change
  total store_read_bytes p50 53417513  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2939.950 ms (load 2523.642 + history 448.129)
    selection          p50     0.092 ms  min     0.066  max     0.508  ratio 0.0% of anchor
    checkpoint_recent  p50  1921.425 ms  min  1737.410  max  1946.480  ratio 65.4% of anchor
    log_read           p50     1.485 ms  min     1.236  max     1.688  ratio 0.1% of anchor
    full_scan          p50   217.713 ms  min   201.910  max   268.919  ratio 7.4% of anchor
    blob_read          p50     0.071 ms  min     0.046  max     0.077  ratio 0.0% of anchor
    graph_replay       p50   169.020 ms  min   160.216  max   204.516  ratio 5.7% of anchor
    metadata_json      p50     0.018 ms  min     0.014  max     0.036  ratio 0.0% of anchor
    messages_json      p50     8.741 ms  min     7.524  max    15.139  ratio 0.3% of anchor
    messages_decode    p50    11.975 ms  min     7.303  max    13.123  ratio 0.4% of anchor
    source_hash        p50    21.365 ms  min    18.880  max    22.316  ratio 0.7% of anchor
    history_rescan     p50   450.581 ms  min   415.813  max   466.259  ratio 15.3% of anchor
    page_index         p50   465.166 ms  min   459.729  max   487.452  ratio 15.8% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184295..42184303 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210246..11210254 (4 distinct values)
  SPREAD   a session_save store_read_bytes 22950..22970 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184299..42184303 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210250..11210254 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22974 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.351/4.143    p95    9.065/11.302   delta  -0.208 ms  control-no-change
  skill_request_overlay p50    5.213/5.448    p95    8.438/24.182   delta  +0.235 ms  control-no-change
  mcp_config_load   p50    0.293/0.269    p95    0.326/0.940    delta  -0.023 ms  control-no-change
  attach_history    p50 3082.699/3047.149 p95 3492.658/3800.369 delta -35.550 ms  control-no-change
  resume_ready      p50  723.775/740.813  p95 1049.301/800.271  delta +17.038 ms  control-no-change
  provider_dispatch p50   12.010/11.045   p95   16.405/19.344   delta  -0.966 ms  control-no-change
  session_save      p50   26.447/27.593   p95   38.753/35.201   delta  +1.146 ms  control-no-change
  total store_read_bytes p50 53417519  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 3082.699 ms (load 2658.946 + history 446.213)
    selection          p50     0.092 ms  min     0.067  max     0.117  ratio 0.0% of anchor
    checkpoint_recent  p50  2213.254 ms  min  1942.356  max  2624.676  ratio 71.8% of anchor
    log_read           p50     1.919 ms  min     1.263  max     2.660  ratio 0.1% of anchor
    full_scan          p50   288.223 ms  min   270.281  max   309.802  ratio 9.3% of anchor
    blob_read          p50     0.061 ms  min     0.056  max     0.197  ratio 0.0% of anchor
    graph_replay       p50   246.830 ms  min   233.659  max   277.156  ratio 8.0% of anchor
    metadata_json      p50     0.034 ms  min     0.022  max     0.039  ratio 0.0% of anchor
    messages_json      p50    15.176 ms  min     6.936  max    17.517  ratio 0.5% of anchor
    messages_decode    p50    12.020 ms  min     8.935  max    12.670  ratio 0.4% of anchor
    source_hash        p50    26.468 ms  min    25.709  max    27.578  ratio 0.9% of anchor
    history_rescan     p50   491.920 ms  min   491.869  max   492.607  ratio 16.0% of anchor
    page_index         p50   541.095 ms  min   427.292  max   551.622  ratio 17.6% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22947..22971 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22951..22963 (5 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (aab096e2fe7c)
stages   /Users/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (3d1557c233c9, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/q2/p1t6rgxj5fj32g1dc7dmd49r0000gn/T/horon-loading-bench-es1xyupa
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.027/0.027    p95    0.029/0.062    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.224/0.234    p95    0.327/0.439    delta  +0.009 ms  control-no-change
  mcp_config_load   p50    0.263/0.275    p95    0.289/0.341    delta  +0.012 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   12.628/12.914   p95   16.447/16.342   delta  +0.286 ms  control-no-change
  total store_read_bytes p50 23673  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22947, 22955, 23657, 23673, 24359, 24375, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22939, 22955, 22963, 23665, 23669, 24371, 24375, 24379]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22947..24379 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22939..24379 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    4.455/4.621    p95    6.895/6.899    delta  +0.166 ms  control-no-change
  skill_request_overlay p50    6.218/6.369    p95    7.965/7.722    delta  +0.151 ms  control-no-change
  mcp_config_load   p50    0.288/0.284    p95    0.378/0.377    delta  -0.004 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   17.262/15.931   p95   46.779/91.498   delta  -1.330 ms  control-no-change
  total store_read_bytes p50 23649  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22947, 22949, 22953, 22955, 23647, 23651, 23655, 24373, 24377, 24381]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22939, 22943, 22951, 23661, 24371, 24375, 24379]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22947..24381 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22939..24379 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.028/0.024    p95    0.053/0.030    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.236/0.225    p95    0.437/0.260    delta  -0.011 ms  control-no-change
  mcp_config_load   p50    0.269/0.266    p95    0.544/0.289    delta  -0.003 ms  control-no-change
  attach_history    p50  102.076/104.610  p95  120.665/128.551  delta  +2.534 ms  control-no-change
  resume_ready      p50   65.010/66.814   p95   72.572/86.695   delta  +1.805 ms  control-no-change
  provider_dispatch p50    1.213/1.239    p95    2.154/1.685    delta  +0.026 ms  control-no-change
  session_save      p50   13.910/16.308   p95   17.983/32.783   delta  +2.398 ms  control-no-change
  total store_read_bytes p50 2841259  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23661, 23681, 24379, 24383, 25101, 25105, 25813, 26515]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23671, 23677, 24359, 24389, 25095, 25105, 25805, 25819, 26525, 26527]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23661..26515 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23671..26527 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.211/4.177    p95    9.222/4.650    delta  -0.034 ms  control-no-change
  skill_request_overlay p50    5.516/5.551    p95   17.503/7.592    delta  +0.035 ms  control-no-change
  mcp_config_load   p50    0.269/0.279    p95    0.379/0.416    delta  +0.010 ms  control-no-change
  attach_history    p50  118.195/120.924  p95  159.530/141.713  delta  +2.730 ms  control-no-change
  resume_ready      p50   65.697/73.167   p95  237.932/97.963   delta  +7.470 ms  control-no-change
  provider_dispatch p50    1.218/1.193    p95    2.393/2.978    delta  -0.026 ms  control-no-change
  session_save      p50   18.827/19.641   p95  165.553/138.366  delta  +0.814 ms  control-no-change
  total store_read_bytes p50 2841257  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23662, 23670, 24376, 24380, 25094, 25104, 25804, 25822, 26528, 26530]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23670, 23674, 24376, 24388, 25088, 25098, 25800, 25810, 26522, 26524]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23662..26530 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23670..26524 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.028/0.029    p95    0.039/0.039    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.217/0.225    p95    0.264/0.271    delta  +0.007 ms  control-no-change
  mcp_config_load   p50    0.249/0.254    p95    0.488/0.299    delta  +0.006 ms  control-no-change
  attach_history    p50 1255.880/1305.271 p95 1464.133/1516.867 delta +49.391 ms  control-no-change
  resume_ready      p50  819.571/797.233  p95  923.099/922.710  delta -22.339 ms  control-no-change
  provider_dispatch p50   15.492/13.294   p95   22.182/27.119   delta  -2.199 ms  control-no-change
  session_save      p50   23.450/22.249   p95   44.443/71.662   delta  -1.201 ms  control-no-change
  total store_read_bytes p50 28640265  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23673, 23685, 24379, 24383, 25089, 25091, 25809, 25815, 26531, 26537]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23655, 23673, 24391, 24393, 25099, 25813, 25817, 26519, 26525]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23673..26537 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23655..26525 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    5.454/4.507    p95   13.742/9.499    delta  -0.947 ms  control-no-change
  skill_request_overlay p50    5.367/5.744    p95   10.622/6.744    delta  +0.378 ms  control-no-change
  mcp_config_load   p50    0.265/0.293    p95    0.423/0.339    delta  +0.028 ms  control-no-change
  attach_history    p50 1253.138/1212.846 p95 1548.474/1501.768 delta -40.292 ms  control-no-change
  resume_ready      p50  841.170/830.585  p95  946.415/979.860  delta -10.585 ms  control-no-change
  provider_dispatch p50   12.622/13.648   p95   17.805/17.513   delta  +1.026 ms  control-no-change
  session_save      p50   24.482/23.253   p95   34.527/52.396   delta  -1.229 ms  control-no-change
  total store_read_bytes p50 28640279  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23659, 23677, 24385, 24387, 25101, 25107, 25815, 25825, 26523, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23677, 24387, 24391, 25093, 25103, 25792, 25811, 26524, 26525]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23659..26529 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23677..26525 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.4 ms | 0.2 ms | 0.4 ms | 5 |
| read (1000 lines) | 0.6 ms | 0.7 ms | 0.5 ms | 0.7 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 0.7 ms | 0.5 ms | 0.7 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.5 ms | 0.3 ms | 0.5 ms | 5 |
| write (small) | 2.1 ms | 2.3 ms | 2.0 ms | 2.3 ms | 5 |
| write (10KB) | 2.6 ms | 3.1 ms | 2.3 ms | 3.1 ms | 5 |
| edit (simple replace) | 2.1 ms | 2.3 ms | 1.6 ms | 2.3 ms | 5 |
| ls (scripts dir) | 0.9 ms | 1.1 ms | 0.8 ms | 1.1 ms | 5 |
| agentgrep (grep in tree) | 3.4 ms | 3.5 ms | 3.2 ms | 3.5 ms | 5 |
| agentgrep (find **/*.rs) | 1.9 ms | 4.2 ms | 1.8 ms | 4.2 ms | 5 |
| bash (true) | 5.6 ms | 6.6 ms | 4.8 ms | 6.6 ms | 5 |
| bash (echo hello) | 5.5 ms | 6.6 ms | 4.7 ms | 6.6 ms | 5 |
| bash (ls fixture tree) | 8.5 ms | 9.7 ms | 8.4 ms | 9.7 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Tool CPU per call

| Tool case | daemon cpu median | min | max | child cpu median | voluntary switches | involuntary switches | windows × calls | source |
|---|---|---|---|---|---|---|---|---|
| read (tiny file) | — | — | — | — | — | — | 0 | — |
| read (1000 lines) | — | — | — | — | — | — | 0 | — |
| read (10 lines @ offset) | — | — | — | — | — | — | 0 | — |
| read (repo Cargo.toml) | — | — | — | — | — | — | 0 | — |
| write (small) | — | — | — | — | — | — | 0 | — |
| write (10KB) | — | — | — | — | — | — | 0 | — |
| edit (simple replace) | — | — | — | — | — | — | 0 | — |
| ls (scripts dir) | — | — | — | — | — | — | 0 | — |
| agentgrep (grep in tree) | — | — | — | — | — | — | 0 | — |
| agentgrep (find **/*.rs) | — | — | — | — | — | — | 0 | — |
| bash (true) | — | — | — | — | — | — | 0 | — |
| bash (echo hello) | — | — | — | — | — | — | 0 | — |
| bash (ls fixture tree) | — | — | — | — | — | — | 0 | — |
| invalid tool (dispatch only) | — | — | — | — | — | — | 0 | — |

CPU unmeasured: no /proc CPU counters for the daemon on this system

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 86.5 ms | 86.5 ms | 86.5 ms | 92.6 ms | 0 |
| 10 | 247.6 ms | 249.4 ms | 249.4 ms | 249.8 ms | 0 |
| 40 | 1.42 s | 1.42 s | 1.42 s | 1.43 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 2.18 s | 2.18 s | 2.18 s | 1 |
| leaf_touch | src/main.rs | 982.8 ms | 982.8 ms | 982.8 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 47.59 s | 47.59 s | 47.59 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 59.14 s | 59.14 s | 59.14 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.23-dev (882db5d) | 14.7 ms | 107.1 MB |
| jcode | jcode v1.0.3 (a0ae514c4) | 16.7 ms | 147.2 MB |
| Claude Code | 2.1.331 (Claude Code) | 44.3 ms | 774.2 MB |
| Codex CLI | codex-cli 0.172.0 | 109.6 ms | 523.2 MB |
| OpenCode | 1.19.6 | 598.3 ms | 609.5 MB |
| pi | 1.6.0 | 313.6 ms | 117.0 MB |
| oh-my-pi (omp) | omp/19.1.1 | 815.2 ms | 317.0 MB |
| oh-my-openagent (omo) | omo 5.1.83 (engine: senpi 2026.11.23) | 231.6 ms | 573.5 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 204.2 ms | 197.7 ms–271.1 ms | baseline | 36.5 MB | baseline | yes | 3,655 / 2,358 (1 tools) | — |
| jcode | 167.6 ms | 160.1 ms–292.5 ms | 1.22× less | 31.6 MB | 1.16× less | yes | 2,878 / 1,111 (1 tools) | — |
| Claude Code | 882.3 ms | 873.3 ms–1.86 s | 4.32× more | 314.5 MB | 8.61× more | yes | 53,206 / 35,357 (14 tools) | — |
| Codex CLI | 2.02 s | 1.85 s–10.60 s | 9.87× more | 256.7 MB | 7.02× more | yes | 42,210 / 19,363 (9 tools) | — |
| OpenCode | 5.73 s | 5.70 s–8.61 s | 28.06× more | 851.5 MB | 23.30× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 537.1 ms | 528.1 ms–618.2 ms | 2.63× more | 126.2 MB | 3.45× more | yes | 5,522 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.55 s | 2.39 s–3.25 s | 12.49× more | 397.3 MB | 10.87× more | yes | 27,599 / 18,203 (11 tools) | — |
| oh-my-openagent (omo) | 23.03 s | 22.68 s–25.88 s | 112.79× more | 649.0 MB | 17.76× more | yes | 31,168 / 2,946 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 207.5 ms | 202.7 ms–265.9 ms | baseline | 36.6 MB | baseline | — | yes | — | 10,988 / 9,033 (8 tools) |
| jcode | — | — | — | — | — | — | — | skipped: jcode has no core tool profile | — |
| Claude Code | — | — | — | — | — | — | — | skipped: Claude Code has no core tool profile | — |
| Codex CLI | — | — | — | — | — | — | — | skipped: Codex CLI has no core tool profile | — |
| OpenCode | — | — | — | — | — | — | — | skipped: OpenCode has no core tool profile | — |
| pi | — | — | — | — | — | — | — | skipped: pi has no core tool profile | — |
| oh-my-pi (omp) | — | — | — | — | — | — | — | skipped: oh-my-pi (omp) has no core tool profile | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | — | skipped: oh-my-openagent (omo) has no core tool profile | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 219.5 ms | 202.2 ms–244.2 ms | baseline | 25.4 MB | baseline | skill-uri | yes | — |
| jcode | 289.6 ms | 265.9 ms–293.1 ms | 1.32× more | 23.7 MB | 1.07× less | skill-tool | yes | — |
| Claude Code | 877.8 ms | 864.2 ms–914.2 ms | 4.00× more | 244.4 MB | 9.64× more | skill-tool | yes | — |
| Codex CLI | 2.46 s | 2.37 s–2.62 s | 11.23× more | 253.7 MB | 10.01× more | file-read | yes | — |
| OpenCode | 8.74 s | 8.64 s–8.81 s | 39.84× more | 934.1 MB | 36.84× more | skill-tool | yes | — |
| pi | 779.0 ms | 760.8 ms–833.7 ms | 3.55× more | 99.6 MB | 3.93× more | file-read | yes | — |
| oh-my-pi (omp) | 3.16 s | 3.12 s–3.33 s | 14.40× more | 367.3 MB | 14.49× more | file-read | yes | — |
| oh-my-openagent (omo) | 24.17 s | 23.38 s–29.39 s | 110.10× more | 630.1 MB | 24.85× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 253.8 ms | 246.0 ms–280.0 ms | baseline | 38.0 MB | baseline | mcp-call | yes | 162.7 ms | — |
| jcode | 381.5 ms | 374.1 ms–392.3 ms | 1.50× more | 35.8 MB | 1.06× less | mcp-action | yes | 254.3 ms | — |
| Claude Code | 946.2 ms | 928.9 ms–973.2 ms | 3.73× more | 270.4 MB | 7.12× more | mcp-tool | yes | 795.8 ms | — |
| Codex CLI | 1.10 s | 1.07 s–1.15 s | 4.35× more | 177.7 MB | 4.68× more | mcp-tool | yes | 887.2 ms | — |
| OpenCode | 11.28 s | 7.71 s–19.52 s | 44.44× more | 743.7 MB | 19.60× more | mcp-tool | yes | 9.33 s | — |
| pi | 837.2 ms | 835.4 ms–856.0 ms | 3.30× more | 121.5 MB | 3.20× more | mcp-tool | yes | 651.9 ms | — |
| oh-my-pi (omp) | 4.75 s | 4.61 s–4.98 s | 18.72× more | 863.6 MB | 22.75× more | mcp-command | yes | 4.40 s | — |
| oh-my-openagent (omo) | 26.41 s | 25.33 s–27.62 s | 104.04× more | 609.8 MB | 16.07× more | mcp-tool | yes | 21.67 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 23.1 MB | 126.1 KB | baseline | — | — | 1 | 0.00 | 31.2 MB | 540.7 KB |
| jcode | 22.8 MB | 97.5 KB | 1.29× less | — | — | 1 | 0.00 | 28.0 MB | 272.6 KB |
| Claude Code | 168.3 MB | 186.8 MB | 1,517× more | — | — | 2 | 1.97 | 212.2 MB | 204.6 MB |
| Codex CLI | 118.9 MB | 92.9 MB | 754.62× more | — | — | 6 | 3.00 | 163.2 MB | 116.3 MB |
| OpenCode | 234.8 MB | 1.7 MB | 13.82× more | — | — | 2 | 0.00 | 443.2 MB | 4.2 MB |
| pi | 92.6 MB | 93.0 MB | 755.06× more | — | — | 2 | 2.00 | 103.3 MB | 102.7 MB |
| oh-my-pi (omp) | 323.1 MB | 83.0 MB | 674.13× more | — | — | 2 | 2.00 | 374.9 MB | 358.7 MB |
| oh-my-openagent (omo) | 149.6 MB | 139.4 MB | 1,132× more | — | — | 4 | 4.00 | 429.4 MB | 445.6 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 31.2 MB | 34.0 MB | 36.0 MB |
| jcode | 28.0 MB | 29.9 MB | 30.5 MB |
| Claude Code | 212.2 MB | 1.1 GB | 2.0 GB |
| Codex CLI | 163.2 MB | 650.4 MB | 1.2 GB |
| OpenCode | 443.2 MB | 646.8 MB | 494.7 MB |
| pi | 103.3 MB | 514.3 MB | 1.0 GB |
| oh-my-pi (omp) | 374.9 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 429.4 MB | 2.2 GB | 4.1 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 2 / 2 | 14 / 14 | 20 / 20 |
| Codex CLI | 6 / 6 | 18 / 18 | 33 / 30 |
| OpenCode | 2 / 2 | 2 / 2 | 2 / 2 |
| pi | 2 / 2 | 10 / 10 | 20 / 20 |
| oh-my-pi (omp) | 2 / 2 | 10 / 10 | 20 / 20 |
| oh-my-openagent (omo) | 4 / 6 | 20 / 38 | 40 / 71 |

**Errors recorded during measurement**

- oh-my-openagent (omo) @ 10 sessions: turn 2 session 7: Connection error.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 7: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.

#### User-installed package cost (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. One turn per sample; samples per scenario: 5. Cells are measured per OS; never compare absolute values across OSes.

| Build | Scenario | Turn (median) | Min–max | First request | Peak tree RSS | Tool round trip | Tools called | vs baseline | Status |
|---|---|---|---|---|---|---|---|---|---|
| candidate | empty | 110.3 ms | 105.9 ms–134.9 ms | 79.8 ms | 22.8 MB | no | – | – | ok |
| candidate | installed | 161.3 ms | 147.4 ms–180.6 ms | 128.2 ms | 23.5 MB | no | – | – | ok |
| candidate | called | 277.9 ms | 271.9 ms–289.1 ms | 135.3 ms | 37.8 MB | yes | ext__hello__greet | – | ok |

**Installed size** (absolute: a size does not need a median)

| Build | Scenario | Package area | Files | Package binary | Agent binary |
|---|---|---|---|---|---|
| candidate | empty | 0.0 B | 0 | 1.1 MB | 107.1 MB |
| candidate | installed | 1.1 MB | 4 | 1.1 MB | 107.1 MB |
| candidate | called | 1.1 MB | 4 | 1.1 MB | 107.1 MB |

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    13.08 ms
  Max:    13.81 ms
  Mean:   13.52 ms
  Median: 13.61 ms
  Stdev:  0.26 ms

--version:
  Min:    12.35 ms
  Max:    13.52 ms
  Mean:   12.66 ms
  Median: 12.51 ms
  Stdev:  0.36 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    21.99 ms
  Max:    67.77 ms
  Mean:   26.93 ms
  Median: 22.53 ms
  Stdev:  14.35 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        1.1       1.1   19.4%
  nofile_limit        1.6       0.5    9.3%
  perm_harden         3.7       2.0   36.5%
  args_parse          3.7       0.0    0.0%
  config_loaded       4.4       0.8   14.1%
  server_new          5.3       0.8   14.7%
  socket_bound        5.6       0.3    6.1%

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~13.6 ms
  Server ready median:     ~22.5 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (0ef1b12672f9)
stages   D:\a\agent\agent\bench-bin\examples\resume_stage_probe.exe (b96e2a792a0c, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-v_lcvadp
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.101/0.101    p95    0.155/0.148    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.748/0.751    p95    0.813/0.822    delta  +0.003 ms  control-no-change
  mcp_config_load   p50    0.925/0.949    p95    0.944/1.429    delta  +0.024 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-drift
  session_save      p50   41.707/41.386   p95   48.541/63.891   delta  -0.321 ms  control-no-change
  total store_read_bytes p50 22952  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22898..22964 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22902..22956 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   14.784/15.033   p95   17.079/15.716   delta  +0.250 ms  control-no-change
  skill_request_overlay p50   37.805/37.581   p95   40.125/41.526   delta  -0.224 ms  control-no-change
  mcp_config_load   p50    0.939/0.959    p95    1.067/1.192    delta  +0.020 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.005/0.007    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   46.973/51.734   p95   59.203/152.654  delta  +4.761 ms  control-no-change
  total store_read_bytes p50 22940  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22916..22956 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22972 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.099/0.100    p95    0.126/0.122    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.781/0.825    p95    0.905/1.068    delta  +0.044 ms  control-no-change
  mcp_config_load   p50    0.929/0.915    p95    0.995/1.056    delta  -0.014 ms  control-no-change
  attach_history    p50  373.208/373.404  p95  386.729/382.050  delta  +0.197 ms  control-no-change
  resume_ready      p50  147.738/153.927  p95  153.172/157.361  delta  +6.190 ms  control-drift
  provider_dispatch p50    1.605/1.575    p95    1.685/1.629    delta  -0.030 ms  control-no-change
  session_save      p50   37.514/38.664   p95   41.261/51.269   delta  +1.150 ms  control-no-change
  total store_read_bytes p50 5311196  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 373.208 ms (load 324.244 + history 48.430)
    selection          p50     0.147 ms  min     0.123  max     0.148  ratio 0.0% of anchor
    checkpoint_recent  p50   269.900 ms  min   269.467  max   270.728  ratio 72.3% of anchor
    log_read           p50     0.161 ms  min     0.160  max     0.220  ratio 0.0% of anchor
    full_scan          p50    25.930 ms  min    25.738  max    25.951  ratio 6.9% of anchor
    blob_read          p50     0.008 ms  min     0.006  max     0.009  ratio 0.0% of anchor
    graph_replay       p50    21.399 ms  min    19.811  max    21.628  ratio 5.7% of anchor
    metadata_json      p50     0.009 ms  min     0.009  max     0.018  ratio 0.0% of anchor
    messages_json      p50     1.522 ms  min     1.414  max     1.556  ratio 0.4% of anchor
    messages_decode    p50     0.932 ms  min     0.912  max     1.051  ratio 0.2% of anchor
    source_hash        p50     0.589 ms  min     0.516  max     0.596  ratio 0.2% of anchor
    history_rescan     p50    48.083 ms  min    48.040  max    52.167  ratio 12.9% of anchor
    page_index         p50    71.371 ms  min    68.924  max    71.440  ratio 19.1% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22908..22972 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22909..22975 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.831/14.871   p95   15.074/15.797   delta  +0.040 ms  control-no-change
  skill_request_overlay p50   37.637/37.949   p95   38.922/45.764   delta  +0.312 ms  control-no-change
  mcp_config_load   p50    0.979/0.958    p95    1.112/1.003    delta  -0.021 ms  control-no-change
  attach_history    p50  381.258/375.264  p95  392.687/392.055  delta  -5.994 ms  control-no-change
  resume_ready      p50  148.748/148.174  p95  156.870/150.643  delta  -0.573 ms  control-no-change
  provider_dispatch p50    1.524/1.521    p95    1.615/1.556    delta  -0.003 ms  control-no-change
  session_save      p50   39.512/39.860   p95   45.605/53.765   delta  +0.348 ms  control-no-change
  total store_read_bytes p50 5311221  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 381.258 ms (load 328.628 + history 49.057)
    selection          p50     0.152 ms  min     0.144  max     0.182  ratio 0.0% of anchor
    checkpoint_recent  p50   271.910 ms  min   270.284  max   279.645  ratio 71.3% of anchor
    log_read           p50     0.162 ms  min     0.161  max     0.170  ratio 0.0% of anchor
    full_scan          p50    26.062 ms  min    26.006  max    26.369  ratio 6.8% of anchor
    blob_read          p50     0.007 ms  min     0.007  max     0.008  ratio 0.0% of anchor
    graph_replay       p50    21.726 ms  min    21.078  max    27.484  ratio 5.7% of anchor
    metadata_json      p50     0.010 ms  min     0.010  max     0.011  ratio 0.0% of anchor
    messages_json      p50     1.493 ms  min     1.459  max     1.734  ratio 0.4% of anchor
    messages_decode    p50     0.966 ms  min     0.929  max     0.995  ratio 0.3% of anchor
    source_hash        p50     0.556 ms  min     0.540  max     0.576  ratio 0.1% of anchor
    history_rescan     p50    49.069 ms  min    48.912  max    55.173  ratio 12.9% of anchor
    page_index         p50    78.110 ms  min    69.764  max    87.714  ratio 20.5% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189153 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099104..1099105 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22934..22972 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189149..4189153 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099101..1099105 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22926..22971 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.099/0.102    p95    0.126/0.143    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.785/0.758    p95    0.857/0.989    delta  -0.027 ms  control-no-change
  mcp_config_load   p50    0.936/0.923    p95    0.973/0.995    delta  -0.013 ms  control-no-change
  attach_history    p50 4118.514/4120.918 p95 4695.200/4213.244 delta  +2.404 ms  control-no-change
  resume_ready      p50 1544.735/1549.684 p95 1600.571/1656.942 delta  +4.950 ms  control-no-change
  provider_dispatch p50   16.291/16.529   p95   17.421/23.435   delta  +0.238 ms  control-no-change
  session_save      p50   37.015/37.509   p95   40.032/139.010  delta  +0.494 ms  control-no-change
  total store_read_bytes p50 53417430  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 4118.514 ms (load 3577.949 + history 541.399)
    selection          p50     0.170 ms  min     0.142  max     0.201  ratio 0.0% of anchor
    checkpoint_recent  p50  3038.236 ms  min  3032.934  max  3086.154  ratio 73.8% of anchor
    log_read           p50     1.680 ms  min     1.654  max     1.708  ratio 0.0% of anchor
    full_scan          p50   263.185 ms  min   262.321  max   265.686  ratio 6.4% of anchor
    blob_read          p50     0.068 ms  min     0.067  max     0.069  ratio 0.0% of anchor
    graph_replay       p50   258.743 ms  min   254.120  max   258.764  ratio 6.3% of anchor
    metadata_json      p50     0.013 ms  min     0.013  max     0.014  ratio 0.0% of anchor
    messages_json      p50    21.004 ms  min    20.856  max    21.265  ratio 0.5% of anchor
    messages_decode    p50    17.675 ms  min    16.317  max    18.419  ratio 0.4% of anchor
    source_hash        p50     4.649 ms  min     4.597  max     4.742  ratio 0.1% of anchor
    history_rescan     p50   548.075 ms  min   543.094  max   552.077  ratio 13.3% of anchor
    page_index         p50   628.817 ms  min   592.548  max   642.320  ratio 15.3% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22924..22983 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22935..22975 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.949/14.813   p95   16.689/15.233   delta  -0.136 ms  control-no-change
  skill_request_overlay p50   37.771/37.476   p95   39.213/40.548   delta  -0.295 ms  control-no-change
  mcp_config_load   p50    0.997/0.970    p95    1.155/1.214    delta  -0.027 ms  control-no-change
  attach_history    p50 4169.681/4145.342 p95 4232.905/4163.177 delta -24.339 ms  control-no-change
  resume_ready      p50 1552.294/1549.378 p95 1559.071/1560.938 delta  -2.916 ms  control-no-change
  provider_dispatch p50   16.886/16.903   p95   18.014/17.679   delta  +0.017 ms  control-no-change
  session_save      p50   37.886/37.735   p95   65.144/121.176  delta  -0.151 ms  control-no-change
  total store_read_bytes p50 53417434  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 4169.681 ms (load 3614.683 + history 553.746)
    selection          p50     0.143 ms  min     0.141  max     0.175  ratio 0.0% of anchor
    checkpoint_recent  p50  3037.647 ms  min  3029.061  max  3053.927  ratio 72.9% of anchor
    log_read           p50     1.763 ms  min     1.645  max     1.791  ratio 0.0% of anchor
    full_scan          p50   261.673 ms  min   261.323  max   264.531  ratio 6.3% of anchor
    blob_read          p50     0.069 ms  min     0.066  max     0.072  ratio 0.0% of anchor
    graph_replay       p50   249.697 ms  min   248.453  max   250.041  ratio 6.0% of anchor
    metadata_json      p50     0.012 ms  min     0.012  max     0.013  ratio 0.0% of anchor
    messages_json      p50    20.504 ms  min    20.394  max    20.559  ratio 0.5% of anchor
    messages_decode    p50    14.886 ms  min    13.874  max    15.028  ratio 0.4% of anchor
    source_hash        p50     4.616 ms  min     4.562  max     4.706  ratio 0.1% of anchor
    history_rescan     p50   539.232 ms  min   529.298  max   540.261  ratio 12.9% of anchor
    page_index         p50   609.732 ms  min   586.083  max   752.401  ratio 14.6% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22914..22975 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22939..22975 (9 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (0ef1b12672f9)
stages   D:\a\agent\agent\bench-bin\examples\resume_stage_probe.exe (b96e2a792a0c, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-7h89n1nj
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.178/0.157    p95    0.216/0.165    delta  -0.020 ms  control-no-change
  skill_request_overlay p50    1.266/1.245    p95    1.400/1.374    delta  -0.020 ms  control-no-change
  mcp_config_load   p50    1.408/1.425    p95    1.479/1.475    delta  +0.017 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.003    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   46.489/47.342   p95   50.662/57.988   delta  +0.853 ms  control-no-change
  total store_read_bytes p50 23628  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22931, 22935, 22947, 22959, 23622, 23634, 23646, 23654, 24300, 24337]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22937, 22941, 22943, 22957, 23606, 23612, 23625, 23646, 24326, 24345]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22931..24337 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22937..24345 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   25.156/24.723   p95   26.477/32.511   delta  -0.433 ms  control-no-change
  skill_request_overlay p50   56.115/56.496   p95   58.992/62.380   delta  +0.381 ms  control-no-change
  mcp_config_load   p50    1.432/1.486    p95    1.527/1.807    delta  +0.054 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   55.199/53.161   p95   65.635/64.149   delta  -2.039 ms  control-no-change
  total store_read_bytes p50 23643  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22931, 22947, 22953, 23626, 23634, 23652, 24327, 24329, 24334, 24341]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22886, 22920, 22933, 23645, 23647, 23656, 24277, 24342, 24356, 24363]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22931..24341 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22886..24363 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.159/0.159    p95    0.196/0.167    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    1.242/1.248    p95    1.352/1.327    delta  +0.005 ms  control-no-change
  mcp_config_load   p50    1.390/1.403    p95    1.436/1.447    delta  +0.013 ms  control-no-change
  attach_history    p50  294.220/296.391  p95  305.800/302.892  delta  +2.171 ms  control-no-change
  resume_ready      p50  234.327/236.190  p95  238.153/246.730  delta  +1.863 ms  control-no-change
  provider_dispatch p50    1.965/1.982    p95    2.523/2.060    delta  +0.017 ms  control-no-change
  session_save      p50   46.747/44.548   p95   49.343/58.656   delta  -2.200 ms  control-no-change
  total store_read_bytes p50 2841108  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23628, 23636, 24355, 24359, 25014, 25047, 25743, 25762, 26439, 26444]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23666, 24349, 24369, 25012, 25066, 25731, 25745, 26462, 26464]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23628..26444 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23666..26464 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   25.424/25.367   p95   28.271/26.375   delta  -0.056 ms  control-no-change
  skill_request_overlay p50   56.392/55.317   p95   61.830/56.412   delta  -1.075 ms  control-no-change
  mcp_config_load   p50    1.465/1.448    p95    1.508/4.089    delta  -0.017 ms  control-no-change
  attach_history    p50  295.038/295.392  p95  308.848/302.797  delta  +0.353 ms  control-no-change
  resume_ready      p50  233.333/231.011  p95  237.589/240.923  delta  -2.323 ms  control-no-change
  provider_dispatch p50    1.999/1.957    p95    2.191/2.076    delta  -0.042 ms  control-no-change
  session_save      p50   46.906/47.741   p95   63.548/50.140   delta  +0.835 ms  control-no-change
  total store_read_bytes p50 2841126  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23629, 23675, 24318, 24364, 25045, 25046, 25736, 25757, 26435, 26440]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23626, 23632, 24332, 24355, 25051, 25067, 25758, 25766, 26449, 26457]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23629..26440 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23626..26457 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.163/0.158    p95    0.221/0.211    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    1.226/1.271    p95    1.321/2.298    delta  +0.045 ms  control-no-change
  mcp_config_load   p50    1.478/1.503    p95    1.543/1.804    delta  +0.025 ms  control-no-change
  attach_history    p50 3060.722/3071.062 p95 3529.557/3278.690 delta +10.340 ms  control-no-change
  resume_ready      p50 2427.967/2430.575 p95 2795.616/2832.819 delta  +2.608 ms  control-no-change
  provider_dispatch p50   22.148/22.341   p95   24.029/25.646   delta  +0.193 ms  control-no-change
  session_save      p50   45.211/46.152   p95   82.796/356.658  delta  +0.940 ms  control-no-change
  total store_read_bytes p50 28640149  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23628, 23676, 24329, 24358, 25048, 25056, 25735, 25759, 26448, 26470]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23658, 23670, 24325, 24361, 25064, 25065, 25752, 25767, 26450, 26459]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23628..26470 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23658..26459 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   24.093/24.230   p95   25.401/24.810   delta  +0.137 ms  control-no-change
  skill_request_overlay p50   56.940/56.194   p95   62.696/60.343   delta  -0.746 ms  control-no-change
  mcp_config_load   p50    1.480/1.472    p95    1.511/1.537    delta  -0.008 ms  control-no-change
  attach_history    p50 3050.229/3061.842 p95 3078.051/3118.689 delta +11.612 ms  control-no-change
  resume_ready      p50 2409.525/2417.812 p95 2431.413/2441.457 delta  +8.287 ms  control-no-change
  provider_dispatch p50   21.699/21.242   p95   22.179/22.563   delta  -0.457 ms  control-no-change
  session_save      p50   44.178/45.932   p95   47.296/53.415   delta  +1.753 ms  control-no-change
  total store_read_bytes p50 28640124  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23658, 24365, 25019, 25036, 25756, 25779, 26450, 26467]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23646, 23666, 24348, 24357, 25016, 25040, 25716, 25767, 26445, 26454]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23658..26467 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23646..26454 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |
| write (small) | 10.1 ms | 10.2 ms | 9.6 ms | 10.2 ms | 5 |
| write (10KB) | 7.6 ms | 10.7 ms | 7.5 ms | 10.7 ms | 5 |
| edit (simple replace) | 10.1 ms | 10.5 ms | 7.7 ms | 10.5 ms | 5 |
| ls (scripts dir) | 2.8 ms | 4.8 ms | 2.3 ms | 4.8 ms | 5 |
| agentgrep (grep in tree) | 5.4 ms | 10.2 ms | 4.8 ms | 10.2 ms | 5 |
| agentgrep (find **/*.rs) | 5.3 ms | 7.5 ms | 4.8 ms | 7.5 ms | 5 |
| bash (true) | 10.6 ms | 18.0 ms | 9.8 ms | 18.0 ms | 5 |
| bash (echo hello) | 10.1 ms | 10.7 ms | 10.0 ms | 10.7 ms | 5 |
| bash (ls fixture tree) | 13.0 ms | 16.2 ms | 12.6 ms | 16.2 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |

#### Tool CPU per call

| Tool case | daemon cpu median | min | max | child cpu median | voluntary switches | involuntary switches | windows × calls | source |
|---|---|---|---|---|---|---|---|---|
| read (tiny file) | — | — | — | — | — | — | 0 | — |
| read (1000 lines) | — | — | — | — | — | — | 0 | — |
| read (10 lines @ offset) | — | — | — | — | — | — | 0 | — |
| read (repo Cargo.toml) | — | — | — | — | — | — | 0 | — |
| write (small) | — | — | — | — | — | — | 0 | — |
| write (10KB) | — | — | — | — | — | — | 0 | — |
| edit (simple replace) | — | — | — | — | — | — | 0 | — |
| ls (scripts dir) | — | — | — | — | — | — | 0 | — |
| agentgrep (grep in tree) | — | — | — | — | — | — | 0 | — |
| agentgrep (find **/*.rs) | — | — | — | — | — | — | 0 | — |
| bash (true) | — | — | — | — | — | — | 0 | — |
| bash (echo hello) | — | — | — | — | — | — | 0 | — |
| bash (ls fixture tree) | — | — | — | — | — | — | 0 | — |
| invalid tool (dispatch only) | — | — | — | — | — | — | 0 | — |

CPU unmeasured: no /proc CPU counters for the daemon on this system

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 212.9 ms | 212.9 ms | 212.9 ms | 213.1 ms | 0 |
| 10 | 673.0 ms | 675.7 ms | 675.7 ms | 676.8 ms | 0 |
| 40 | 2.10 s | 2.82 s | 2.82 s | 2.83 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 505.8 ms | 505.8 ms | 505.8 ms | 1 |
| leaf_touch | src\main.rs | 651.2 ms | 651.2 ms | 651.2 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 25.54 s | 25.54 s | 25.54 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 35.55 s | 35.55 s | 35.55 s | 1 |

## TUI (`horon-tui`)

대화형 클라이언트는 별도 바이너리이고 따로 잽니다. 이 수치는 `tui` 묶음에서 나오며 위의 헤드리스 표와 비교하지 않습니다. `first_frame_ms` 와 `direct_first_frame_ms` 는 서로 다른 두 프로그램을 각각 띄운 값이고, 핸드오프는 그 둘의 차이입니다.

![TUI (`horon-tui`)](charts/tui-01-latency.ko.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.ko.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 36.3 ms | 34.2 ms–39.4 ms | 5 |
| First frame (direct `horon-tui`) | 29.9 ms | 27.9 ms–30.9 ms | 5 |
| TTY handoff cost | 6.3 ms | 5.4 ms–10.7 ms | 5 |
| Key press to echo (p95) | 0.3 ms | 0.1 ms–0.7 ms | 50 |
| Idle RSS after 5 s | 7.7 MB | 7.7 MB–7.8 MB | 5 |
| RSS after one turn | 7.9 MB | 7.8 MB–7.9 MB | 5 |
| Frame draw time (p50) | 1.0 ms | 1.0 ms–1.0 ms | 5 |
| Frame draw time (p95) | 1.0 ms | 1.0 ms–1.6 ms | 5 |
| Install size (`horon-tui`) | 10.1 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Held frame draw (p95) | 0.5 ms | 0.5 ms–0.9 ms | 5 |
| Scroll key handling (p50) | 0.8 µs | 0.7 µs–0.9 µs | 5 |
| Scroll key handling (p95) | 1.1 µs | 1.0 µs–2.3 µs | 5 |
| First held frame after scrolling | 0.6 ms | 0.6 ms–0.7 ms | 5 |
| Key dispatch through completed draw (p50) | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Key dispatch through completed draw (p95) | 0.5 ms | 0.5 ms–0.9 ms | 5 |
| First key dispatch through completed draw | 0.6 ms | 0.6 ms–0.7 ms | 5 |
| Held frame redraw without input (p50) | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Held frame redraw without input (p95) | 0.5 ms | 0.5 ms–0.5 ms | 5 |

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 104.6 ms | 87.2 ms–424.6 ms | 5 |
| First frame (direct `horon-tui`) | 75.6 ms | 57.6 ms–134.5 ms | 5 |
| TTY handoff cost | 43.3 ms | -39.8 ms–349.0 ms | 5 |
| Key press to echo (p95) | 0.3 ms | 0.2 ms–2.8 ms | 50 |
| Idle RSS after 5 s | 10.3 MB | 10.2 MB–10.3 MB | 5 |
| RSS after one turn | 10.6 MB | 10.5 MB–10.6 MB | 5 |
| Frame draw time (p50) | 0.8 ms | 0.8 ms–0.9 ms | 5 |
| Frame draw time (p95) | 0.9 ms | 0.9 ms–1.6 ms | 5 |
| Install size (`horon-tui`) | 7.4 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame draw (p95) | 0.4 ms | 0.4 ms–1.0 ms | 5 |
| Scroll key handling (p50) | 0.6 µs | 0.6 µs–1.2 µs | 5 |
| Scroll key handling (p95) | 2.0 µs | 1.4 µs–6.7 µs | 5 |
| First held frame after scrolling | 0.7 ms | 0.6 ms–0.8 ms | 5 |
| Key dispatch through completed draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Key dispatch through completed draw (p95) | 0.4 ms | 0.4 ms–1.0 ms | 5 |
| First key dispatch through completed draw | 0.7 ms | 0.6 ms–0.8 ms | 5 |
| Held frame redraw without input (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame redraw without input (p95) | 0.6 ms | 0.4 ms–0.8 ms | 5 |

### Windows

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | — | — | — |
| First frame (direct `horon-tui`) | — | — | — |
| TTY handoff cost | — | — | — |
| Key press to echo (p95) | — | — | — |
| Idle RSS after 5 s | — | — | — |
| RSS after one turn | — | — | — |
| Frame draw time (p50) | 2.5 ms | 2.5 ms–2.6 ms | 5 |
| Frame draw time (p95) | 2.7 ms | 2.6 ms–2.9 ms | 5 |
| Install size (`horon-tui`) | 9.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.5 ms | 0.4 ms–0.5 ms | 5 |
| Held frame draw (p95) | 0.5 ms | 0.5 ms–0.7 ms | 5 |
| Scroll key handling (p50) | 0.7 µs | 0.7 µs–0.8 µs | 5 |
| Scroll key handling (p95) | 1.5 µs | 1.3 µs–3.2 µs | 5 |
| First held frame after scrolling | 0.8 ms | 0.7 ms–0.8 ms | 5 |
| Key dispatch through completed draw (p50) | 0.5 ms | 0.4 ms–0.5 ms | 5 |
| Key dispatch through completed draw (p95) | 0.5 ms | 0.5 ms–0.7 ms | 5 |
| First key dispatch through completed draw | 0.8 ms | 0.7 ms–0.8 ms | 5 |
| Held frame redraw without input (p50) | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Held frame redraw without input (p95) | 0.5 ms | 0.5 ms–0.7 ms | 5 |

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
