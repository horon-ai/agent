# 서버 구조

## 개요

horon agent는 **서버 하나, 클라이언트 여럿** 구조를 쓴다. 서버 프로세스 하나가 모든 세션과 상태를 관리한다. 클라이언트 쪽은
TypeScript와 Rust SDK다. 이들은 Unix 소켓으로 서버에 붙고 연결이 끊기거나 서버가 다시
읽힌 뒤에도 투명하게 다시 연결한다. 이 저장소에는 클라이언트 구현이 없다.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              SERVER (🔥 blazing)                              │
│                                                                             │
│  horon-agent serve                                                                │
│  ├── Unix socket:  /run/user/$UID/horon-agent.sock                                │
│  ├── Debug socket: /run/user/$UID/horon-agent-debug.sock                          │
│  ├── Registry:     ~/.horon-agent/servers.json                                    │
│  ├── Provider (OpenAI-compatible, direct /chat/completions)                 │
│  ├── MCP pool (shared across sessions)                                      │
│  └── Sessions:                                                              │
│        ├── 🦊 fox   (active)  → "🔥 blazing 🦊 fox"                         │
│        ├── 🐻 bear  (active)  → "🔥 blazing 🐻 bear"                        │
│        └── 🦉 owl   (idle)    → "🔥 blazing 🦉 owl"                         │
└─────────────────────────────────────────────────────────────────────────────┘
         │              │              │
         ▼              ▼              ▼
    ┌─────────┐   ┌─────────┐   ┌─────────┐
    │ Client 1│   │ Client 2│   │ Client 3│
    │ 🦊 fox  │   │ 🐻 bear │   │ 🦉 owl  │
    └─────────┘   └─────────┘   └─────────┘
```

## 이름 짓기

```
SERVER = Adjective/Verb modifier          SESSIONS = Animal nouns
────────────────────────────              ────────────────────────
🔥 blazing   ❄️ frozen   ⚡ swift          🦊 fox    🐻 bear   🦉 owl
🌀 rising    🍂 falling  🌊 rushing        🌙 moon   ⭐ star   🔥 fire
✨ bright    🌑 dark     💫 spinning       🐺 wolf   🦁 lion   🐋 whale

Combined: "🔥 blazing 🦊 fox" = server + session
```

서버는 시작할 때 무작위 형용사나 동사 이름을 받는다(예: "blazing"). 세션마다 동물 명사를 받는다(예: "fox"). 둘이 합쳐져 화면에
표시되는 자연스러운 구절이 된다: "🔥 blazing 🦊 fox".

서버 이름은 레지스트리(`~/.horon-agent/servers.json`)를 통해 다시 읽힌 뒤에도 유지된다. 서버가 `/reload`에서 새
바이너리로 exec하면 새 프로세스가 새 이름으로 등록한다. 낡은 항목은 자동으로 정리된다.

## 수명 주기

```
  START                          CONNECT                     RELOAD
  ─────                          ───────                     ──────
  horon-agent (first run)              horon-agent (subsequent)          /reload
       │                              │                          │
       ├─▶ No server? Spawn daemon    ├─▶ Server exists?         ├─▶ Server execs into
       ├─▶ Wait for socket            │   Connect directly       │   new binary (same PID)
       ├─▶ Connect as client          │                          ├─▶ All clients disconnect
       └─▶ Create session             └─▶ Create/resume session  └─▶ Clients auto-reconnect
```

### 서버 시작

`horon-agent`를 실행하면 서버가 이미 도는지 확인한다.

1. **서버가 있으면:** 클라이언트로 바로 연결한다
2. **서버가 없으면:** `horon-agent serve`를 `setsid`로 분리된 데몬으로 띄우고, 소켓을 기다린 뒤 연결한다

서버는 `setsid()`로 띄운 클라이언트에서 완전히 분리되므로 어떤 클라이언트를 죽여도 서버나 다른 클라이언트에 영향이 없다.

오래 도는 배포에서는 `horon-agent serve --server-name <name>`이나 `HORON_AGENT_SERVER_NAME`
환경변수로 데몬에 클라이언트가 볼 수 있는 안정적인 정체성을 줄 수 있다. 표시용 이름을 선호하는 서비스 관리자를 위해 선택적
`HORON_AGENT_SERVER_DISPLAY_NAME` 환경변수도 받는다. CLI 입력이 환경변수 입력을 이긴다. 이름은 레지스트리에 안전한 소문자
이름표로 정규화되므로 `mount-cloud/fabian`은 `mount-cloud-fabian`으로 표시된다.

### 서버 종료

서버는 다음 경우에 종료한다.
- **유휴 기한:** 클라이언트가 없고 살아 있는 헤드리스 swarm 워커도 없이 5분이 지날 때. 공유 서버 기한은 300초로 고정돼 있으며(`crates/app-core/src/server.rs`의 `IDLE_TIMEOUT_SECS`), `[server]`로 설정할 수 없다. 감시자가 10초마다 확인하므로 종료가 5분보다 조금 늦을 수 있다.
- **수동:** 서버 프로세스를 죽일 때
- **다시 읽기:** 서버가 새 바이너리로 exec할 때(소켓 경로는 같다)

디버그 제어 서버는 공유 서버 유휴 감시를 끈다. 임시 서버는 별도 수명 정책을 쓴다.

### 헤드리스 `run` 세션과 임시 서버

`horon-agent run`은 서버를 전혀 쓰지 않는다. 프로세스 안에서 `Agent`를 만들고 메시지 하나를 보낸 뒤 끝난다.
`--socket <path>`는 도구가 어느 소켓으로 접속할지만 고르므로 대부분의 실행에서는 그 경로를 아무도 열지 않는다.

정말로 서버가 필요한 도구가 하나 있다. `swarm`은 워커가 서버가 맡는 헤드리스 세션이기 때문에 소켓으로 데몬과 이야기한다. 예전에는 `run`
세션에서 이런 요청이("horon-agent server is not running") 그대로 실패해서 headless 자동화가 swarm을 쓸 수
없었다. 이제 `run` 명령이 요청 시 서버를 띄우는 등록기(
`crate::server_spawn::register_on_demand_server_spawner`)를 걸어 두고, 서버에 닿지 못하는 첫 swarm
요청이 서버를 하나 띄운다. 공유 소켓에서는 평범한 오래 사는 데몬이 되고, 사용자 지정 `--socket`에서는 그 실행이 소유하는 서버가 된다.

```
horon-agent run --socket /tmp/run.sock "spawn a worker that ..."
   │
   ├─▶ agent runs in-process; no server started
   ├─▶ swarm tool dials /tmp/run.sock → not found
   ├─▶ spawn `horon-agent serve --temporary-server --owner-pid <run pid>`
   │      on /tmp/run.sock, using the *current* executable
   └─▶ retry the request; the server exits within ~10s of the run
```

알아 둘 만한 성질은 다음과 같다.

- **게으르다.** `swarm`을 쓰지 않는 실행은 비용을 치르지 않는다. `horon-agent run` 시작은 그대로다.
- **소켓을 따른다.** 사용자 지정 `--socket`에만 그 실행이 소유하는 임시 서버가 붙는다. `--socket`이 없으면 실행은 평범한 공유 데몬을 준비한다. 공유 경로에 임시 서버를 두면 뒤에 오는 모든 클라이언트가 쓰는 소켓을 점유했다가 실행이 끝날 때 사라지기 때문이다.
- **소유한다.** 임시 수명 감시자는 `--owner-pid`가 사라지면 서버를 종료하므로 일회성 실행이 데몬을 남기지 않는다. 소켓, 디버그 소켓, 데몬 잠금, 빌드 해시 곁파일, 메타데이터 파일이 모두 제거된다. 워커는 그것을 띄운 실행이 사는 동안만 산다. 워커를 띄우고 기다리지 않고 끝나는 `run`은 설계상 워커를 잃는다.
- **현재 바이너리.** 소유 경로는 공유 서버 갱신 후보가 아니라 `std::env::current_exe()`를 띄우므로, 로컬에서 빌드한 바이너리를 `--socket`으로 분리해 확인하면 그 바이너리가 응답한다(AGENTS.md의 "Verifying a change at runtime" 참고).
- **`run` 전용.** 대화형 실행과 클라이언트 실행은 요청 시 띄우기 등록기를 걸지 않는다. 이미 자기 시작 경로로 데몬을 준비하며, 도구 깊은 곳에서 촉발한 실행이 그 로직과 경쟁하게 되기 때문이다.

### 세션 소유 표시 파일(`active_pids`)

`~/.horon-agent/active_pids/<session_id>`에는 세션을 소유한 프로세스의 PID가 들어 있다. 서버 모드에서는 데몬
PID이므로 여러 세션이 같은 PID를 공유할 수 있다. 디렉터리 이름과 달리 "활성"은 프로세스 소유를 뜻하지 터미널 창이 열려 있거나 클라이언트가
붙어 있거나 모델이 생성을 돌리고 있다는 뜻이 아니다. 데몬이 쥔 세션은 클라이언트가 끊긴 뒤에도 등록된 채 남을 수 있다.

표시 파일은 세션을 닫거나 지울 때 제거된다. 프로세스가 끝난 뒤 남을 수도 있으므로 표시 파일만으로 소유자가 살아 있다고 증명되지 않는다.
`active_session_ids()`는 PID 생존을 확인하지 않고 표시 파일 이름만 나열하는 반면, `session_presence()`는 죽은
소유자를 걸러낸다. 이 표시 파일은 수명과 충돌 복구를 돕지 창 추적을 하지 않는다. 별도 `streaming_pids` 표시 파일이 모델 응답 생성을
추적한다.

### 원격 클라이언트 작업 디렉터리

기본적으로 클라이언트는 구독할 때 현재 작업 디렉터리를 서버로 보내고 서버가 그것을 세션 작업 디렉터리로 쓴다. 원격 데몬용 소켓 전달 래퍼는
`--remote-working-dir`로 클라이언트 경로와 서버 경로를 분리할 수 있다.

```bash
horon-agent --socket /tmp/horon-agent.sock -C /local/checkout --remote-working-dir /remote/checkout
```

`-C` 경로는 클라이언트에 있어야 한다. `--remote-working-dir`은 서버에 있는 절대 경로여야 한다.

### 클라이언트 재연결

클라이언트에는 재연결 루프가 들어 있다. 연결이 끊기면(서버 다시 읽기, 네트워크 문제 등) 다음 순서로 동작한다.

1. 클라이언트가 "Connection lost - reconnecting..."을 보여 준다
2. 지수 백오프로 다시 시도한다(1초, 2초, 4초... 최대 30초)
3. 다시 연결되면 같은 세션을 이어 간다(세션 상태는 디스크에 남는다)
4. 서버가 다시 읽혔다면 더 새 클라이언트 바이너리가 있을 때 클라이언트 자신도 다시 exec할 수 있다

### 핫 리로드(`/reload`)

1. 클라이언트가 서버로 `Request::Reload`를 보낸다
2. 서버가 요청한 클라이언트로 `Reloading` 이벤트를 보낸다
3. 서버가 `serve` 인자로 새 바이너리에 `exec()`를 건다
4. 새 서버 프로세스가 같은 소켓에서 시작한다
5. 모든 클라이언트가 자동으로 다시 연결한다
6. 시작한 클라이언트도 자기 바이너리가 낡았으면 다시 exec한다

## 소켓 경로

```
/run/user/$UID/
├── horon-agent.sock              # Main communication socket
├── horon-agent-debug.sock        # Debug/testing socket
└── horon-agent-daemon.lock       # "one daemon on horon-agent.sock" flock
```

`--socket`이나 `HORON_AGENT_SOCKET`으로 경로를 바꾸면 곁 잠금 파일(`<socket>.daemon.lock`)도 따로
생기므로, 분리된 서버가 소켓을 잡아도 공유 데몬은 계속 돈다. 데몬 하나는 런타임 디렉터리가 아니라 *소켓* 하나에 붙는다. 띄우기 잠금은 늘 소켓
범위였고 이제 데몬 잠금도 같아졌다.

## 재적재

`/reload`와 `horon-agent server reload`는 서버를 새 바이너리로 exec하고 클라이언트가 다시
연결한다. 재적재 지점은 `shared-server` 채널이며, 세션 구분 없이 같은 경로를 쓴다. 재적재
도중 중단된 세션은 `reload_recovery` 안내로 이어서 진행한다.

## 주요 동작

| 상황 | 동작 |
|----------|----------|
| 첫 `horon-agent` 실행 | 서버 데몬을 띄우고 연결한다 |
| 이후 `horon-agent` 실행 | 이미 있는 서버에 연결한다 |
| 클라이언트 하나 죽이기 | 서버와 다른 클라이언트는 영향받지 않는다 |
| `/reload` | 서버가 새 바이너리로 exec하고 클라이언트가 다시 연결한다 |
| 모든 클라이언트 닫기 | 살아 있는 헤드리스 swarm 워커 없이 5분이 지나면 공유 서버가 유휴 기한으로 종료한다(디버그 제어가 켜져 있으면 예외) |
| 세션 재개 | `horon-agent --resume fox`가 기존 세션에 다시 연결한다 |
| `horon-agent run` | 에이전트가 프로세스 안에서 돌고 도구가 서버를 필요로 하지 않으면 서버를 쓰지 않는다 |
| `horon-agent run` + `swarm` | 공유 데몬을 준비하거나, `--socket`이 사용자 지정이면 그 실행이 소유한 임시 서버를 띄운다 |
