# 호스팅과 사용자별 경계

언어: [English](HOSTING.md) | **한국어**

호스팅 배포는 고객마다 에이전트 컨테이너 또는 Fly Machine 한 개와 홈 볼륨 한 개를 두고, 두 고객을 한 데몬에 넣지 않습니다. 이 문서는 플래그 계약, 이미지와 볼륨 배치, 컨트롤 플레인이 소유하는 라우팅 계약, 정지와 삭제 절차, 남아 있는 한계를 정리합니다.

## 호스팅 플래그 계약

호스팅 모드의 유일한 권위는 환경변수입니다. 고객은 자기 `config.toml`을 쓸 수 있으므로, 파일이 실행 중인 모드를 넓힐 수 있어서는 안 됩니다.

| 값 | 출처 | 뜻 |
| --- | --- | --- |
| 호스팅 모드 | `HORON_AGENT_HOSTING_MODE` | `per-user-container` 또는 `shared-worker`입니다. 변수가 없으면 호스팅이 아닙니다. 다른 값은 허용된 두 값을 알려 주는 시작 오류입니다 |
| 테넌트 id | `HORON_AGENT_TENANT_ID` | 검증된 감사 라벨입니다. 소문자 ASCII 글자, 숫자, 하이픈으로 1바이트부터 63바이트까지이며 글자나 숫자로 시작하고 끝납니다. 디렉터리 이름과 로그 라벨로만 쓰이고 세션 조회 키가 아닙니다 |
| `[hosting] allow_shared_children` | `config.toml` | `false`이면 허용되는 모드에서도 MCP 서버와 확장 자식 프로세스를 거부합니다. `true`는 금지한 모드를 넓히지 못합니다 |
| `[hosting] allowed_mcp_servers` | `config.toml` | 호스팅 프로세스가 연결할 수 있는 MCP 서버 이름입니다. 값이 없으면 모두 허용하고, 빈 목록이면 하나도 허용하지 않으며, 이름은 정확히 일치해야 합니다. 모드를 넓히지 못하고 호스팅이 아닌 프로세스는 그대로이며, 자세한 내용은 [호스팅 모드의 MCP·확장 격리](../safety/SECURITY.ko.md)에 있습니다 |
| `[hosting] mode` | `config.toml` | 경고 한 번과 함께 무시합니다. 대신 `HORON_AGENT_HOSTING_MODE`를 설정합니다 |

해석된 정책은 프로세스 하나에 하나인 값입니다. `HostingPolicy::mode()`, `tenant_id()`, `is_hosted()`, `is_per_user_container()`, `is_shared_worker()`, `allows_shared_children()`, `allowed_mcp_servers()`, `permits_mcp_server()`, `inherits_operator_logins()`가 지원되는 조회이고, MCP·확장 게이트와 비로컬 노출 검사는 환경변수를 다시 읽지 않고 이 값을 씁니다.

모드마다 MCP 서버와 패키지 자식을 어떻게 다루는지, `HOSTED_POLICY` 감사 이벤트와 게이트의 한계는 [호스팅 모드의 MCP·확장 격리](../safety/SECURITY.ko.md)에 있습니다.

## 시작 경계

사용자별 컨테이너는 아래 조건을 모두 만족하지 않으면 시작을 거부합니다. 데몬은 소켓을 열기 전에 검사하고, 실패한 조건을 이름으로 알리는 메시지와 함께 종료합니다.

| 조건 | 이유 |
| --- | --- |
| `HORON_HOME`이 설정되어 있고, 절대 경로이며, 존재하고, 디렉터리이고, 소유자 전용(`0700`)이며, 심볼릭 링크가 아님 | 고객의 상태는 그 컨테이너만 들어갈 수 있는 디렉터리에 있어야 합니다 |
| `HORON_HOME`이 `$HOME/.horon`이 아님 | 호스팅 고객이 운영자의 홈에서 서비스되어서는 안 됩니다 |
| `HORON_AGENT_RUNTIME_DIR`이 설정되어 있고, 절대 경로이며, 소유자 전용이고, 홈 안이 아님 | 소켓은 볼륨 밖에 두고 런타임 디렉터리는 영속 상태와 분리합니다 |
| `HORON_AGENT_TENANT_ID`가 설정되어 있고 유효함 | 모든 호스팅 프로세스가 감사 라벨을 가집니다 |
| `[gateway] enabled`가 거짓임 | TCP 게이트웨이는 컨테이너 소켓 경계 밖 주소를 엽니다 |

공유 워커는 이 검사 없이 시작해서 자체 게이트가 실행하면 안 되는 동작을 거부하게 합니다. `config.toml`이 무엇을 말하든 고객을 위한 자식 프로세스와 원격 MCP 연결은 절대 허용하지 않습니다.

## 이미지, 볼륨, 런타임 배치

`scripts/hosting/Containerfile`은 릴리스 이미지와 같은 게시 릴리스 아카이브로 호스팅 이미지를 만듭니다. UID `10001`을 유지하고, `/data`를 볼륨으로 선언하며, `HORON_HOME=/data/home`과 `HORON_AGENT_RUNTIME_DIR=/run/horon`을 설정하고, 호스팅 변수를 검증한 뒤 `horon-agent --no-update serve`를 실행하는 `scripts/hosting/entrypoint.sh`로 시작합니다.

같은 이미지는 Fly Machine에서도 실행되며, `/data`에 마운트한 Fly Volume이 그 고객의 홈입니다. 다만 아래 Machine 설정이 이미지가 필요로 하는 종료 신호와 타임아웃, 유휴 정책을 채워 줄 때입니다. 경계에 필요한 것은 볼륨과 환경, TCP 게이트웨이가 없다는 사실이지 실행 환경이 아니므로, 로컬 Docker 호스트와 다른 것은 이미지가 아니라 Machine 설정입니다.

| 경로 | 내용 |
| --- | --- |
| `/data` | 테넌트 볼륨입니다. 고객마다 한 개입니다. 볼륨 루트는 비어 있지 않습니다. Fly가 루트 소유의 `lost+found`를 만들므로 빈 루트를 가정하지 않습니다 |
| `/data/home` | `HORON_HOME`입니다. 세션, 트랜스크립트, 승인, MCP 설정, 환경설정, 기억, 사용자 파일이 있습니다 |
| `/run/horon` | 소켓 디렉터리입니다. 볼륨과 분리되어 있고 시작할 때마다 다시 만듭니다 |

이미지는 에이전트 포트를 노출하지 않습니다. API 소켓과 데몬 소켓은 컨테이너 안의 파일이고, 컨테이너 밖 클라이언트는 컨트롤 플레인이나 라우팅 계약에 나오는 인증된 TCP 리스너를 통해서만 접근합니다.

## 라우팅 계약

컨트롤 플레인은 고객에서 인스턴스와 볼륨으로 가는 매핑을 소유합니다. 에이전트 바이너리는 요청에서 테넌트 id를 읽지 않고 고객 사이를 라우팅하지 않습니다.

1. 컨트롤 플레인이 고객을 인증하고 `user -> instance -> volume`을 해석합니다.
2. 그 고객의 인스턴스를 시작하거나 깨우거나 재사용합니다. `scripts/hosting/instance.sh create`는 볼륨을 준비하고, `start`는 슈퍼바이저 아래에서 데몬을 포그라운드로 실행하며, 컨테이너 런타임은 볼륨을 `/data`에 마운트한 이미지를 시작합니다.
3. 고객의 요청을 그 인스턴스로 전달합니다. 한 호스트 안에서는 인스턴스의 유닉스 소켓으로 갑니다. Fly 너머에서는 유닉스 소켓에 닿을 수 없으므로, 요청은 데몬이 Machine 안에 여는 **인증된 TCP 리스너**로 가야 하고, 이 리스너는 어떤 요청도 처리하기 전에 모든 연결을 인증합니다. 인증 없는 엔드포인트는 어디에서도 닿을 수 없으므로, "소켓으로 전달"은 네트워크를 넘는 설계가 아니라 한 호스트에서의 지름길입니다.
4. 두 고객의 요청은 한 프로세스에 도달하지 않습니다. 공유 프로세스가 없으므로 검사할 세션별 테넌트 검사도 없습니다.

호스팅 이미지는 게이트웨이를 끕니다. 하네스 API를 자기 밖으로 서비스해야 하는 컨테이너는
`horon-agent api-bridge --api-bind`(또는 `HORON_AGENT_API_BIND`)로 시작하고, 그 테넌트 홈
안에서 자격 증명을 발급하며, 앞단의 TLS 종료 프록시를 확인합니다. 그러면 리스너가 모든 TCP
연결을 인증하고 로컬 소켓은 기존 핸드셰이크를 유지합니다(`docs/protocol/HARNESS_API.ko.md`).
자격 증명 파일이 테넌트의 `HORON_HOME`에 있으므로 자격 증명은 구조적으로 고객별입니다.
리스너 계약 전체는 `docs/protocol/HARNESS_API.ko.md`에 있습니다.

배포 대상은 Fly Machines이며, 고객마다 Machine 하나와 `/data`로 마운트한 Fly Volume 하나를
씁니다. Fly Proxy와 6PN은 유닉스 소켓에 닿을 수 없으므로 위 리스너가 Machine을 떠나는 유일한
길이고, 계약이 허용하는 유일한 길입니다.

| 경로 | 닿는 방식 | 인증 |
|---|---|---|
| 공개 포트 | `services` 항목의 `internal_port`가 `--api-bind` 포트와 같고, Fly Proxy가 443에서 TLS를 종료한 뒤 WireGuard 메시로 평문을 전달하므로 운영자가 `--api-insecure-behind-proxy`를 준다 | 테넌트의 베어러 토큰 |
| 사설 6PN | 다른 Machine이나 컨트롤 플레인이 테넌트의 6PN 주소나 `<machine>.vm.<app>.internal`로 접속한다. 리스너는 `[::]:PORT`나 `fly-local-6pn`에 바인드한다 | 테넌트의 베어러 토큰 |

6PN은 조직 단위로 사설이지 사용자 단위로 사설이 아니므로 토큰을 대신하지 못합니다.
`fly-local-6pn`과 `[::]`은 IPv6 주소이므로 루프백이 아닌 바인드이고 같은 확인을 요구합니다.
정지되거나 유휴 상태인 Machine에는 프로세스가 없어 리스너도 없습니다. 자격 증명 파일은 볼륨에
남고, 리스너는 유휴 시간으로 스스로 끝나지 않으므로 autostop만이 이를 끝냅니다. 이런 플랫폼
서술은 Fly.io 문서(`docs.fly.io/networking/tls-termination`,
`docs.fly.io/networking/app-services`, `docs.fly.io/networking/private-networking`)를 따르며,
이 저장소는 Fly API 호출도 프록시 헤더 신뢰도 구현하지 않습니다.

## 정지, 깨우기, 복원

인스턴스를 유휴 상태로 두는 일은 컨테이너를 멈추거나 데몬에 `SIGTERM`이나 `SIGINT`를 보내는 것입니다. 데몬은 종료 전에 진행 중인 세션 저장을 비우고 등록 해제와 런타임 소켓 정리를 마치므로(`docs/architecture/SERVER.md`), 정지는 영속적인 정지이고 다음 시작은 같은 볼륨을 엽니다.

- `scripts/hosting/instance.sh stop <tenant>`, `docker stop`, 슈퍼바이저의 종료 신호로 멈춥니다. `horon-agent-sdk`의 `stop_hosted_instance`가 같은 동작입니다. 신호를 보내고, 엔드포인트가 접속을 받지 않을 때까지 기다린 뒤에야 테넌트를 정지됐다고 보고합니다. Fly에서는 방어선으로 `SIGTERM` 고정을 계속 권장하지만, 플랫폼 기본값인 `SIGINT`에서도 같은 저장 비우기와 정리가 동작합니다.
- 같은 볼륨으로 이미지나 데몬을 다시 시작해 깨웁니다. 세션, 승인, MCP 설정, 기억 파일은 볼륨에서 읽고, 세션은 일반 재개 경로로 이어집니다.
- 테넌트 사이에서 볼륨을 복사하지 않고, 한 볼륨을 두 인스턴스에 마운트하지 않습니다.
- 유휴 상태인 Machine은 볼륨을 그대로 두고 잃는 것이 없습니다. 콜드 스타트는 루트 파일시스템을 초기화하므로 `/run/horon`을 다시 만들고, 데몬은 바인드 전에 남은 소켓을 지웁니다.

## Fly Machines: 고객당 Machine 하나와 Volume 하나

컨트롤 플레인은 고객마다 Fly App 하나를 두고, 그 안에 Machine 하나와 Fly Volume 하나를 둡니다. 이 저장소는 Fly API 호출을 구현하지 않습니다. 컨트롤 플레인(`horon-ai/server`)이 모든 공급자 호출을 소유하고, 이 절은 그것이 지켜야 할 계약입니다.

### Machines / Volume / 6PN 매핑

| Fly 개념 | 이 계약에서 |
| --- | --- |
| App | 고객마다 하나입니다. 다중 테넌트 App 하나로는 사용자별 Machine에 라우팅할 수 없습니다 |
| Machine | 고객마다 하나이며, 아래 환경으로 호스팅 이미지를 실행합니다 |
| Volume | 고객마다 하나이며 그 Machine에 `/data`로 붙습니다. 리전 하나, 호스트 하나이고, 한 볼륨은 한 번에 한 Machine에만 붙습니다 |
| 6PN | 컨트롤 플레인이 접속하는 사설 주소 공간입니다. 주소는 재부팅과 마이그레이션 때 바뀌므로 `<machine>.vm.<app>.internal`을 DNS로 해석합니다 |
| `HORON_HOME` | `/data/home`이며 볼륨 안에 있습니다 |
| `services` | 인증된 TCP 리스너가 돌기 전까지 비어 있습니다 |

### 이미지가 요구하는 Machine 설정

| 설정 | 값 | 이유 |
| --- | --- | --- |
| `stop_config.signal` | `SIGTERM`(권장) | 데몬은 `SIGTERM`뿐 아니라 Fly의 기본 종료 신호인 `SIGINT`에서도 진행 중인 세션 저장을 비우고 등록 해제와 소켓·잠금 정리를 마친 뒤 종료 코드 0으로 끝납니다. 다른 슈퍼바이저와 일관된 방어선으로 `SIGTERM` 명시를 계속 권장합니다. `fly.toml`: `kill_signal = "SIGTERM"` |
| `stop_config.timeout` | 최소 `30`초 | Fly의 기본 `kill_timeout`은 5초이며, 저장 비우기와 등록 해제, 종료를 합친 시간보다 짧습니다. `fly.toml`: `kill_timeout = 30` |
| `restart.policy` | `no`, 또는 데몬이 동의하는 유휴 종료 정책과 짝지은 명시적 `on-failure` | 데몬은 클라이언트가 300초 없으면 종료 코드 44로 끝납니다. `on-failure`는 이를 다시 띄워 재시작이 반복됩니다. 유휴의 주인을 하나로 정합니다. 데몬 자체 종료와 `restart = "no"`를 쓰거나, 호스팅 유휴 종료 스위치와 `auto_stop = "suspend"`를 씁니다 |
| `env` | `HORON_AGENT_HOSTING_MODE=per-user-container`, `HORON_AGENT_TENANT_ID=<tenant>`, `HORON_HOME=/data/home`, `HORON_AGENT_RUNTIME_DIR=/run/horon`, `HORON_AGENT_GATEWAY_ENABLED=0`, `HORON_AGENT_NO_AUTO_UPDATE=1` | 위 시작 경계입니다 |
| `guest` | 공유 CPU 1개, 메모리 1GB | suspend는 최대 2GB와 스왑 없음을 요구합니다 |
| `mounts` | 고객의 볼륨을 `/data`에 | 유일한 영속 상태입니다 |

### 스냅숏, 복원, 삭제

| 작업 | 공급자 호출 순서 | 참고 |
| --- | --- | --- |
| 백업 | `horon volume quiesce` 후 `fly volumes snapshots create <volume>` | quiesce는 기억 인덱스를 체크포인트하고 페이지 파일을 fsync하며 쓰기를 막습니다. 아래를 보십시오 |
| 복원 | `fly volumes create --snapshot-id <snapshot>`으로 새 볼륨을 만들고, 대체 Machine을 만들어 시작한 뒤 `horon volume resume` | 스냅숏은 같은 크기 이상의 **새** 볼륨으로만 복원되고 기존 볼륨으로는 되돌릴 수 없습니다. 중앙 `user_volumes.volume_id`가 새 id로 옮겨집니다 |
| 삭제 | `delete_user`: 쓰기 막기, 키 폐기, 모든 스냅숏 삭제, Machine 삭제, 볼륨 삭제, 중앙 행 `deleted` 표시 | 붙어 있는 볼륨은 삭제할 수 없으므로 Machine을 먼저 지웁니다. 삭제는 영구적이고, 삭제된 볼륨 id는 24시간 뒤, 스냅숏은 보존 기한까지 남습니다 |

이 절차가 의존하는 스냅숏 사실은 다음과 같습니다. 기본은 하루 한 번 스냅숏이고 보존 기한은 1일에서 60일(기본 5일)이며, Fly는 파일시스템 동결이나 크래시 일관성 보장을 **문서로 약속하지 않습니다**. 부하 중에 찍은 스냅숏은 기껏해야 크래시 일관성으로 다룹니다.

### quiesce 요구

`horon volume quiesce`는 살아 있는 저장소의 크래시 일관성 스냅숏이 페이지 꼬리를 잃거나 다음 열기에서 다시 만들어야 하는 인덱스를 남길 수 있어서 있습니다. 저장소의 기록자 잠금을 잡고, SQLite write-ahead 로그를 `TRUNCATE`로 체크포인트하며, 인덱스와 모든 페이지 파일을 fsync하고, 쓰기를 거부하게 만드는 펜스를 씁니다. 체크포인트 토큰과 저장소 리비전을 보고하며, 멱등입니다. 데이터 손실 구간은 명시적입니다. **quiesce 없이 찍은 스냅숏은 승인된 마지막 몇 초의 쓰기를 놓칠 수 있고 첫 열기에서 전체 재색인이 필요할 수 있으며**, 성공한 quiesce 뒤에 찍은 스냅숏은 호출이 반환되기 전에 저장소가 승인한 어떤 것도 놓치지 않습니다. `horon volume resume`이 펜스를 풉니다. 스냅숏과 복원은 트래픽을 받는 볼륨에서 `resume`이 끝나야 완료입니다.

### 컨트롤 플레인 인터페이스(문서 전용)

```yaml
# 사용자마다 Fly App 하나입니다. 다중 테넌트 App 하나로는 사용자별 Machine에 라우팅할 수 없습니다.
config:
  image: registry.fly.io/horon-agent-hosting@sha256:<digest>
  guest: {cpu_kind: shared, cpus: 1, memory_mb: 1024}   # suspend는 2GB 이하를 요구합니다
  env:
    HORON_AGENT_HOSTING_MODE: per-user-container
    HORON_AGENT_TENANT_ID: <tenant-id>
    HORON_HOME: /data/home
    HORON_AGENT_RUNTIME_DIR: /run/horon
    HORON_AGENT_GATEWAY_ENABLED: "0"
    HORON_AGENT_NO_AUTO_UPDATE: "1"
  mounts: [{volume: vol_<id>, path: /data}]
  services: []                    # 인증된 TCP 리스너가 생긴 뒤에만 추가합니다
  stop_config: {signal: SIGTERM, timeout: "30s"}
  restart: {policy: "no"}         # 또는 합의한 유휴 종료 정책과 함께 on-failure
  auto_destroy: false
  metadata: {tenant: <tenant-id>, user: <user-id>}
# fly.toml 대응: kill_signal="SIGTERM", kill_timeout=30,
# auto_stop_machines="off"|"suspend", auto_start_machines=true,
# min_machines_running=0, internal_port=<listener-port>
```

**수명 호출:** 볼륨을 만들고(리전, 크기, 보존 1일에서 60일, 예약 스냅숏), Machine을 만들고(볼륨 id, 환경, 서비스 없음), `started`를 기다리고, 서비스하고, 정비를 위해 `cordon`하고, `SIGTERM`으로 멈추고, quiesce한 뒤 스냅숏을 찍고, `--snapshot-id`로 새 볼륨에 복원하고, 새 볼륨으로 대체 Machine을 만들어 시작한 뒤, 이전 Machine을 지우고 마지막으로 이전 볼륨을 지웁니다.

**책임 분담.** 이 저장소는 안정된 환경과 배치 계약, `SIGTERM`·`SIGINT` 저장 비우기, 준비 상태(소켓이 접속을 받음), 쓰기 펜스와 quiesce 명령, 기억 볼륨 내보내기·가져오기·`delete_user` 단계를 제공합니다. Fly API 호출, 볼륨·Machine·사용자 매핑, 이미지 게시, 리전과 배치, TLS와 도메인, 고객 인증, 할당량과 과금, 재시도와 멱등성, 스냅숏 예약은 `horon-ai/server`에 남깁니다. 에이전트는 절대 Fly API를 호출하지 않습니다.

## 고객 삭제

삭제 경로는 둘이고 서로 바꿔 쓸 수 없습니다.

- **로컬·개발.** `delete_hosted_instance`와 `scripts/hosting/instance.sh delete <tenant>`는 공유 상위 디렉터리 아래 `<root>/<tenant>` 호스트 디렉터리 하나를 지웁니다. 로컬 Docker나 슈퍼바이즈드 프로세스 런타임을 위한 참고 구현입니다. `delete_hosted_instance`는 디렉터리를 지우는 것이지 Fly 볼륨을 지우는 것이 아니며, 이 함수도 `instance.sh`도 Fly API를 호출하거나 스냅숏을 지우지 않습니다.
- **Fly 호스팅.** `horon volume delete-user <user_id>`가 아래의 영속적이고 재개 가능한 단계 기계를 돌립니다. 쓰기를 막고, 사용자 데이터 키를 폐기하고, 로컬 저장소를 지우고, 모든 스냅숏을 지우고, Machine과 볼륨을 지우고, 마지막에 중앙 행을 `deleted`로 표시합니다.

| 단계 | 실행 주체 | 결과 |
| --- | --- | --- |
| `write_stop` | `delete_user`(로컬) | 기억 저장소가 펜스되고 페이지와 인덱스가 지워집니다 |
| `key_revoke` | `delete_user`(로컬, 키 서비스) | 키 레코드에 `revoked_at`이 남고 준비된 자재가 지워집니다 |
| `snapshot_delete` | 컨트롤 플레인 / 공급자 | 볼륨의 모든 스냅숏이 삭제됩니다 |
| `volume_delete` | 컨트롤 플레인 / 공급자 | Machine을 지운 뒤 볼륨을 지웁니다 |
| `mark_deleted` | 컨트롤 플레인 | 중앙 행이 `deleted`로 바뀝니다 |

작업 파일은 `<HORON_HOME>/volume-state/deletion-job.json`에 있고 기억 저장소 밖입니다. 저장소를 지우는 단계가 무슨 일을 했는지에 대한 기록을 잃지 않게 하려는 것입니다. 실패한 단계는 이유와 함께 기록되고 명령은 0이 아닌 코드로 끝나며, `horon volume delete-user`를 다시 실행하면 그 단계에서 이어집니다. `deleted`는 모든 단계가 끝난 뒤에만 보고되고, 일부만 끝난 실행은 `deleting`과 해당 단계를 보고합니다.

한 고객을 지워도 다른 고객은 그대로여야 합니다. 세션, 볼륨, 실행 중인 데몬이 분리되어 있고, 경계 통합 테스트가 데몬 두 개를 띄워 정확히 그것을 확인합니다.

## 범위 밖

- 과금, 요금제, 할당량.
- 오토스케일과 배치 정책.
- 중앙 메타데이터 서비스. 매핑은 컨트롤 플레인이 소유하고 이 저장소는 구현하지 않습니다. 그 서비스에 대한 에이전트 쪽 클라이언트와 요청 형태는 기억 볼륨 문서(`docs/runtime/MEMORY.ko.md`)에 있고, 서비스 자체는 `horon-ai/server#28`이며 계약은 아직 초안입니다.
- Fly API 클라이언트. 공급자를 호출하는 것은 컨트롤 플레인이고, 에이전트는 필요한 볼륨 명령만 노출합니다.
- 페이지 파일과 내보내기 tar의 봉투 암호화. 키 계약(메타데이터, 폐기, 폐기 시 열기 거부)은 구현했지만, 잠금 파일에 없는 크레이트가 필요해 암호 구현은 하지 않았습니다.
- 호스팅 유휴 종료 스위치. 공유 서버 유휴 기한을 바꾸거나 호스팅 유휴 종료 스위치를 두는 일은 이 범위에 포함하지 않습니다.
- 하네스 API가 이미 구현한 인증된 TCP 리스너를 넘어서는 비로컬 노출 인증.
- 애플리케이션 코드 경로, 세션, 와이어 프로토콜에 테넌트 id를 넣는 작업.
- 컨테이너 경계를 넘어서는 범용 OS 샌드박스.

## 호환성

`HORON_AGENT_HOSTING_MODE`가 없으면 아무것도 바뀌지 않습니다. 데몬은 운영자의 홈에서 서비스하고, 게이트웨이 기본값은 꺼져 있고, 기존 명령이 모두 이전과 같게 동작합니다. 호스팅 모드는 시작 검사와 런처를 더할 뿐 세션, 와이어 프로토콜, `[hosting]` 밖의 설정 의미를 바꾸지 않습니다.

볼륨 명령(`horon volume ...`)은 추가적입니다. `HORON_HOME`이 가리키는 볼륨에 작용하고, 운영자가 실행하지 않으면 아무것도 바꾸지 않습니다. 쓰기 펜스는 quiesce만 쓰는 파일이므로, 펜스가 없는 저장소는 이전과 똑같이 동작하고, 펜스가 막는 것은 기억 저장소 자체의 쓰기뿐입니다.
