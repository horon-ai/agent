# 프롬프트와 토큰

시스템 프롬프트 구성과 토큰 추정 방식을 다룬다.

## 시스템 프롬프트 설정

horon-agent는 여러 계층으로 시스템 프롬프트를 만든다. 그중 두 계층은 사용자가 편집하는
파일이라 다시 빌드하지 않고 에이전트 동작을 조정할 수 있다.

### 계층 (순서대로)

1. **기본 시스템 프롬프트** — 내장 `crates/base/src/prompt/system_prompt.md`이며
   파일로 재정의할 수 있다(아래 참고).
2. 기능 모듈(예: Mermaid 안내).
3. 제품별 self-dev 안내. HoronAgent Desktop 체크아웃을 뿌리로 한 세션은 CLI·TUI self-dev
   플래그, `selfdev`, `debug_socket`과 별개로 Desktop 프롬프트와 `desktop_selfdev` 도구를
   자동으로 받는다.
4. `AGENTS.md` — 프로젝트 `./AGENTS.md`와 전역 `~/AGENTS.md`. 이 계층이 다루지 않는 하위 디렉터리의 `AGENTS.md`는 `[tools] nested_agents_md`가 켜져 있으면 `read` 결과에 따로 붙는다([`docs/runtime/TOOLS.md`](TOOLS.md)).
5. 프롬프트 오버레이 — `./.horon-agent/prompt-overlay.md`와 `~/.horon-agent/prompt-overlay.md`.
6. 선호 도구 — `./.horon-agent/preferred-tools.md`와 `~/.horon-agent/preferred-tools.md`.
7. 메모리와 활성 스킬 프롬프트(동적이며 캐시하지 않는다).

### 안내 추가 (가장 흔한 작업)

기본 프롬프트를 건드리지 않고 지시를 덧붙인다.

- `~/.horon-agent/prompt-overlay.md` — 모든 곳에 적용된다.
- `./.horon-agent/prompt-overlay.md` — 프로젝트 하나에 적용된다.

둘 다 있으면 함께 포함된다. 4~6 계층에서 프로젝트 경로와 전역 경로가 같은 정규 경로로
해석되면(예: `$HOME`에서 작업하거나 심링크 별칭을 쓰는 경우) 파일은 프로젝트 제목 아래에
한 번만 포함된다. 서로 다른 파일은 내용이 같더라도 둘 다 포함된다. 전역 `.horon-agent`
디렉터리는 `HORON_AGENT_HOME`이 설정되어 있으면 그 값을 따른다. 4 계층은 작업 디렉터리와
전역 파일만 넣고, 그 아래 디렉터리의 `AGENTS.md`는 프롬프트가 아니라 `read` 결과에 붙는다
(`[tools] nested_agents_md`, 기본값 켜짐, `HORON_AGENT_NESTED_AGENTS_MD`로 덮어씀).

### 기본 프롬프트 교체

1 계층을 완전히 교체하려면 다음 파일 중 하나를 만든다.

- `./.horon-agent/system-prompt.md`(프로젝트, 가장 높은 우선순위)
- `~/.horon-agent/system-prompt.md`(전역)

비어 있지 않은 첫 파일이 이긴다. 없으면 내장 기본값을 쓴다. 비어 있거나 공백만 있는 파일은
기본값으로 돌아가므로 실수로 빈 프롬프트를 배포할 수 없다.

이것은 기본 프롬프트만 교체한다. AGENTS.md, 오버레이, 스킬, 메모리는 여전히 적용된다.

### 스트리밍 중 규칙 강제 ([[stream_rules]])

운영자가 금지 문구를 `[[stream_rules]]`로 선언하면, 응답이 흘러가는 동안 어시스턴트 텍스트를 그 패턴과 대조한다. 맞으면 응답을 버리고, 규칙 본문을 시스템 리마인더로 넣고, 같은 대화 상태에서 요청을 다시 낸다. 모델은 자기가 어긴 규칙을 보고 텍스트를 다시 만든다.

`[features] stream_rules`가 꺼져 있으면 매처를 아예 만들지 않는다. 기본값은 꺼짐이다(`crates/base/src/config/default_file.rs`).

```toml
[features]
stream_rules = true

[[stream_rules]]
id = "no-apology-opener"
pattern = "(?i)^(I'm sorry|Sorry,)"
body = "사과로 시작하지 말고 바로 결론부터 쓰세요."
```

규칙 하나는 `id`, `pattern`(Rust `regex`), `body` 세 필드를 갖는다(`crates/config-types/src/lib.rs`). 동작에는 의도한 제약이 네 가지 있다(`crates/app-core/src/agent/stream_rules.rs`).

- 이미 매치된 규칙은 그 턴에서 다시 시도하지 않는다. 같은 문구를 반복하는 모델이 주입 예산을 다 써 버리지 않게 한다.
- 어시스턴트 텍스트만 검사한다. 도구 인자는 별도 스트림 이벤트로 오므로 규칙에 넘기지 않는다.
- 스캔 범위는 마지막 4 KiB(`LOOKBACK_BYTES`)다. 두 청크에 걸친 문구는 잡고, 긴 응답에서 토큰마다 전체 재스캔하는 비용은 피한다.
- 한 턴에 주입은 세 번까지다(`MAX_INJECTIONS`). 네 번째로 다른 규칙이 맞으면 턴을 이어 가지 않고 멈춘다.

규칙은 다음 선택 필드로 범위와 빈도를 좁힌다. 모두 생략하면 기존 동작(매 턴, 텍스트,
중단 후 재시도)과 같다.

| 필드 | 값 | 뜻 |
|---|---|---|
| `scope` | `text`(기본), `tool`, `tool:<name>` | 무엇을 검사할지. 비어 있으면 `text` |
| `paths` | 글롭 목록 | 도구 호출의 `file_path`/`path` 인자에 적용. 비어 있으면 모든 경로 |
| `repeat` | `turn`(기본), `once`, `after-gap` | 매 턴, 세션당 한 번, `repeat_gap` 턴마다 한 번 |
| `repeat_gap` | 정수(기본 10) | `after-gap`의 최소 턴 간격 |
| `delivery` | `interrupt`(기본), `passive` | 작업을 막고 다시 시키거나, 리마인더만 덧붙임 |

`tool` 범위는 도구 호출 인자를 압축 JSON으로 직렬화해 패턴과 대조한다. `interrupt`면 호출을
실행하지 않고 `Blocked by rule "{id}": {body}`를 도구 오류 결과로 돌려준다. `passive`면 호출을
그대로 실행하고 결과 끝에 `<system-reminder>Rule "{id}": {body}</system-reminder>`를 붙인다.
텍스트 범위의 `passive` 규칙은 응답을 끊지 않고, 다음 요청의 시스템 리마인더에 합쳐진다.

`once`와 `after-gap`이 실제로 발동한 턴은 세션 보조 상태의 `stream_rule_ledger`에 규칙 id별로
기록되므로, 세션을 다시 열어도 같은 규칙이 다시 발동하지 않는다.

주입 지점 표시는 `S`다(`STREAM_RULE_POINT`). 소프트 인터럽트의 A~D 지점과 구분되므로 클라이언트가 내용을 보지 않고도 어느 쪽인지 안다.

### 다른 에이전트 규칙 파일 (rulebook 모드)

`[features] foreign_rules = true`는 `.cursor/rules/*.mdc`, `.cursorrules`, `.clinerules`,
Copilot instructions, Windsurf rules, `GEMINI.md`를 읽는다. 기본 `foreign_rules_mode = "inline"`은
모든 본문을 시스템 프롬프트에 넣는다.

`foreign_rules_mode = "rulebook"`은 `alwaysApply` 규칙만 본문을 인라인하고 나머지는 경로, 설명,
적용 글롭만 `# Rulebook` 목록으로 보여 준다. 나머지 규칙의 본문은 `read`·`edit`·`write`·
`hashline_edit`·`apply_patch`가 해당 글롭에 맞는 파일을 처음 건드릴 때
`<project-rule path="…">` 블록으로 한 번만 붙는다. 규칙 파일 경로를 미리 읽지 않아도 되고,
관련 없는 규칙이 컨텍스트를 차지하지 않는다.

### 참고

- 이 파일들의 변경은 **새 세션**에 적용된다. 실행 중인 세션은 시작할 때 캡처한 프롬프트를
  유지한다.
- 내장 `system_prompt.md`를 편집하려면 다시 빌드해야 한다(`selfdev build-reload`).
  `include_str!`로 내장되기 때문이다.
- Swarm 모델 라우팅 안내에는 비슷한 전용 파일이 있다. `.horon-agent/swarm-prompt.md`다.
  `/swarm-prompt`로 활성 프로젝트 파일이나 전역 파일을 편집한다. 새 에이전트는 최신 내용을
  즉시 읽고, 이미 실행 중인 에이전트는 세션 생성 시 캡처한 프롬프트를 유지해 도구 정의와
  컨텍스트 캐시가 안정적으로 남는다.

### SDK 직접 재정의

SDK 호출자는 세션을 만들 때 파일을 쓰지 않고 **조립된 시스템 프롬프트 전체**를 교체할 수
있다.

```typescript
const session = await client.createSession({
  workingDir: process.cwd(),
  systemPrompt: "You are a concise programming tutor.",
});
```

Rust 호출자는 `create_session_with_options(CreateSessionOptions { working_dir: None, system_prompt: Some("You are a concise programming tutor.".into()) })`를 쓴다. 기존 `create_session(working_dir)` API도 그대로 쓸 수 있다.

기본 계층만 교체하는 `system-prompt.md`와 달리 이 옵션은 조립된 프롬프트 계층 전체를
교체한다. 옵션을 생략하면 평소 HoronAgent 프롬프팅을 유지한다. 빈 문자열은 빈 시스템
프롬프트를 명시적으로 고른다. 재정의는 그 세션에 속하고 재개를 위해 영속되며 포크에
상속된다. 기존 세션에 붙는 것은 그 프롬프트를 바꾸지 않는다. 이 기능은 `system_prompt`
세션 생성 필드를 지원하는 데몬 버전을 요구한다.

## 토큰 추정

컨텍스트가 얼마나 남았는지 재는 추정기와, 그 추정치가 어디서 쓰이는지를 설명합니다. 압축은 프로바이더가 사용량을 보고하기 전에 대화의 크기를 재야 하는데, 그 자리에서 문자 수를 4로 나누는 계산은 영어 산문 밖에서 방향이 갈렸습니다. 이 문서는 지금 트리의 비율 상수, 실행 단위 모델, 측정 대역, 그리고 남는 오차를 적습니다.

### 바이트/4가 틀린 이유

같은 저장소 파일을 실제 토크나이저(o200k_base, cl100k_base)로 세고 기존 `바이트/4`와 비교한 값입니다. 측정은 tiktoken 0.14.0으로 했습니다.

| 콘텐츠 | o200k 오차 | cl100k 오차 | 그 오차가 부르는 결과 |
| --- | --- | --- | --- |
| Rust 소스 | +13.4% | +13.5% | 실제보다 크게 잡아 아직 쓸 수 있는 컨텍스트를 일찍 버립니다. |
| 한국어 문서 | −7.0% | −31.4% | 적게 잡아 창을 넘긴 요청을 보냅니다. |
| JSON | −32.0% | −32.3% | 적게 잡아 압축이 늦고, 프로바이더가 요청을 거절합니다. |
| TOML | −9.9% | −9.5% | 적게 잡는 쪽입니다. |

두 방향이 모두 손해입니다. 과대 추정은 멀쩡한 대화를 지우고, 과소 추정은 요청이 창을 넘어 400 응답을 받습니다. 그래서 추정기는 문자 수 하나가 아니라 글 종류별 비율로 셉니다.

### 부류별 비율

모든 문자는 정확히 한 부류에 들어갑니다. 어느 부류에도 안 들어가 0토큰이 되는 문자는 없습니다. 비율 상수는 `crates/token-estimate/src/lib.rs`에 있습니다.

| 부류 | o200k | cl100k | 위치 |
| --- | --- | --- | --- |
| 한글 | 0.78 | 1.22 | `crates/token-estimate/src/lib.rs:129` |
| 한자 | 0.95 | 1.90 | `crates/token-estimate/src/lib.rs:141` |
| 가나 | 0.95 | 1.04 | `crates/token-estimate/src/lib.rs:151` |
| 아랍 문자 | 0.38 | 0.88 | `crates/token-estimate/src/lib.rs:159` |
| 인도계 문자 | 0.38 | 1.20 | `crates/token-estimate/src/lib.rs:168` |
| 그 밖의 알파벳 | 0.30 | 0.50 | `crates/token-estimate/src/lib.rs:177` |
| 비ASCII 기호 | 0.90 | 1.00 | `crates/token-estimate/src/lib.rs:186` |
| 이모지 | 2.00 | 2.00 | `crates/token-estimate/src/lib.rs:196` |
| ASCII 영숫자 | 4.87자/토큰 | 4.87자/토큰 | `crates/token-estimate/src/lib.rs:73` |
| ASCII 문장부호 | 0.55 | 0.55 | `crates/token-estimate/src/lib.rs:77` |

두 토크나이저 계열은 한글에서 가장 크게 갈립니다. o200k는 흔한 한국어 부분 문자열을 한 토큰으로 합치고, cl100k는 음절마다 거의 한 토큰을 씁니다. 그래서 같은 문장이 o200k에서 44토큰, cl100k에서 72토큰입니다.

### 단어 실행 단위

문자는 부류별로 세지만, 그 합을 그대로 쓰지 않습니다. 텍스트를 공백으로 나눈 실행(run)마다 따로 계산합니다 (`crates/token-estimate/src/lib.rs:284`). 같은 문자라도 단어 안에 묶여 있을 때 토큰이 적게 들기 때문입니다. 문자 하나에 고정 가중치를 주는 선형 모델로는 Rust 소스와 JSON의 기준점을 동시에 맞출 수 없습니다.

- 실행 하나는 최소 1토큰입니다. 빈 문자열도 1을 돌려줍니다 (`crates/token-estimate/src/lib.rs:213`). 0을 돌려주면 호출자가 공짜로 읽습니다.
- ASCII 문장부호는 실행의 본문 위에 더합니다. 괄호와 쉼표는 단어보다 훨씬 잘게 쪼개지기 때문입니다.
- 한글이 섞인 실행은 문장부호를 다시 더하지 않습니다. 한글 음절이 이미 실행의 대부분을 차지하고, 문장부호를 또 더하면 한국어 표본이 과대 추정되는 것으로 측정됐습니다.
- 문장부호가 실행 글자의 절반 이상이면 기호 밀집 실행으로 봅니다. 이 실행의 ASCII 영숫자는 산문 비율로 평균 내지 않고, 구분자 사이 조각마다 `ceil(길이 / 4.87)`(최소 1토큰)으로 셉니다 (`crates/token-estimate/src/lib.rs` `SEGMENT_DENSITY_PERCENT`). 임계를 25%나 35%로 낮추면 짧은 JSON 기준 표본이 실측보다 30% 크게 잡혀, 50%로 정했습니다. 이 규칙을 넣으며 `TOKEN_ESTIMATE_VERSION`을 `script-aware-v2`로 올렸습니다.

### CJK 병합 할인

한자와 가나는 실행 길이에 따라 할인율이 달라집니다 (`crates/token-estimate/src/lib.rs:120`).

```
merge = max(1 - 18.0 / cjk_len, min_merge)
```

`CJK_MERGE_WINDOW`는 18.0입니다 (`crates/token-estimate/src/lib.rs:99`). 할인 바닥은 부류마다 따로 있습니다. 한자는 0.53 (`crates/token-estimate/src/lib.rs:105`), 가나는 0.70 (`crates/token-estimate/src/lib.rs:112`)입니다.

이 장치가 푸는 문제는 하나의 비율로는 산문과 반복을 함께 맞출 수 없다는 점입니다. 산문에서는 글자가 이웃과 합쳐져 싸게 붙고, 같은 글자를 길게 반복하면 합칠 이웃이 없어 글자마다 거의 한 토큰이 듭니다. 비율을 산문에 맞추면 반복이 과소 추정되고, 반복에 맞추면 산문이 과대 추정됩니다. 그래서 비율은 산문에 맞추고, 짧은 실행이 산문 할인을 그대로 받지 않도록 바닥을 둡니다.

바닥값은 측정에서 나왔습니다. 두 부류 모두 o200k 비율이 0.95이므로 바닥은 (짧은 실행의 실측 문자당 비용) / 0.95입니다. 한자는 0.50 / 0.95 = 0.53, 가나는 0.667 / 0.95 = 0.70입니다.

### 역변환 상한이 따로 있는 이유

압축 파이프라인에는 토큰 예산을 문자 예산으로 되돌리는 자리가 있습니다. 이 역변환은 추정기를 재사용하지 않고 고정 상한을 씁니다. 값은 4이고 (`crates/compaction-core/src/lib.rs`), 이름은 `CHARS_PER_TOKEN_UPPER_BOUND`입니다.

역할이 둘이라 상수도 둘입니다. 추정 결과를 만드는 쪽은 추정기이고, 그 추정기가 어떤 규칙으로 만들어졌는지는 `TOKEN_ESTIMATE_VERSION`으로 따로 표시합니다. 역변환은 추정기의 비율을 거꾸로 쓰지 않고 자기 상한을 유지합니다. 추정기는 적합된 비율이라 역함수가 깔끔하지 않고, 역변환의 몫은 "이 토큰 예산에 들어가는 문자 수"의 상한을 보장하는 것입니다. 그 상한을 추정기 자신에게 물으면 상한이 재려는 값에 의존하게 됩니다.

### 측정 대역

일곱 산문 표본을 두 계열에서 측정했습니다. 오차는 실측 대비입니다.

| 표본 | 글자 | o200k 추정 | o200k 실측 | 오차 | cl100k 추정 | cl100k 실측 | 오차 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 한자 산문 | 50 | 32 | 29 | +10.3% | 60 | 56 | +7.1% |
| 붙여쓴 한자 | 47 | 28 | 28 | +0.0% | 55 | 51 | +7.8% |
| 가나 산문 | 70 | 49 | 47 | +4.3% | 63 | 65 | −3.1% |
| 키릴 | 125 | 35 | 32 | +9.4% | 55 | 52 | +5.8% |
| 아랍 | 95 | 31 | 29 | +6.9% | 70 | 69 | +1.4% |
| 데바나가리 | 61 | 20 | 21 | −4.8% | 61 | 66 | −7.6% |
| 한국어 산문 + 이모지 4개 | 33 | 22 | 22 | +0.0% | 30 | 32 | −6.2% |

일곱 표본이 두 계열 모두 ±10.3% 안에 들어옵니다. 이 표는 짧은 실행 바닥값을 넣기 전 커밋에서 측정한 값이고, 바닥값을 넣은 수정은 짧은 실행만 바꾸므로 산문 값은 그대로입니다.

### 알려진 한계

| 한계 | 측정 |
| --- | --- |
| 같은 CJK 글자를 길게 반복하면 과소 추정합니다. 비율은 산문에 맞춰져 있고, 실제 토크나이저는 반복을 글자마다 쪼갭니다. | 한자 1,000자를 933으로 추정(6.7% 과소), 가나 250자(실측 240)를 220으로 추정(8.3% 과소) |
| JSON 모양 콘텐츠는 여전히 오차가 큽니다. 기호 밀집 실행 규칙으로 압축된(minified) JSON은 거의 맞지만, 들여쓴 JSON은 아직 과소 추정합니다. | `crates/token-estimate/tests/fixtures`의 같은 JSON 문서(tiktoken 0.14.0, o200k): 들여쓴 판 1,412 실측에 −13.4%, 압축판 1,094 실측에 +2.5%. 테스트 허용치는 14%입니다. |

### 실측 보정

추정기 오차는 압축 관리자가 경로마다 따로 보정합니다 (`crates/base/src/compaction/mod.rs`). 요청을 보낼 때 그 요청의 추정치를 기록하고(`note_request_estimate`), 프로바이더가 입력 토큰 사용량을 보고하면(`update_observed_input_tokens`) `실측 / 추정` 표본 하나를 만듭니다.

- 추정치가 4,096토큰 미만인 요청은 표본으로 쓰지 않습니다. 고정 오버헤드가 비율을 좌우하기 때문입니다.
- 표본 하나는 0.6–1.8로 자르고, 기존 비율과 `0.7 × 기존 + 0.3 × 표본`으로 섞습니다.
- 컨텍스트 계산(`effective_token_count_with`)은 추정치에 이 비율을 곱하되 0.75 아래로 내리지 않고, 그 값과 마지막 실측 중 큰 쪽을 씁니다.
- 프로바이더나 모델, 인증 경로가 바뀌면 비율을 버립니다. 토크나이저가 달라질 수 있기 때문입니다.
- 현재 비율은 `[compaction/state]` 로그의 `calibration_ratio`로 볼 수 있습니다.
