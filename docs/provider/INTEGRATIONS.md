# 외부 연동

원격 MCP 서버, 사용자 정의 웹 검색, Google OAuth 직접 설정을 다룬다.

## 원격 MCP 서버

HoronAgent는 로컬 stdio 서버, Streamable HTTP 서버, 레거시
HTTP+SSE 서버에 연결한다. 기존 stdio 설정은 계속 동작한다.

### 설정

원격 서버는 `~/.horon-agent/mcp.json`이나 프로젝트의 `.mcp.json`에 추가한다.

```json
{
  "mcpServers": {
    "remote": {
      "type": "http",
      "url": "https://mcp.example.com/mcp"
    },
    "remote-with-token": {
      "type": "http",
      "url": "https://mcp.example.com/mcp",
      "headers": {
        "Authorization": "Bearer ${MCP_TOKEN}"
      }
    },
    "legacy": {
      "type": "sse",
      "url": "https://mcp.example.com/sse"
    }
  }
}
```

`http`와 `streamable-http`는 Streamable HTTP를 고른다. URL만 있고
command나 명시적 type이 없는 항목도 Streamable HTTP를 고른다. `sse`는
레거시 전송을 명시적으로 고른다. 초기화 과정에서 엔드포인트가 Streamable
HTTP를 지원하지 않는다고 나오면 HTTP 연결이 레거시 SSE로
폴백할 수 있다. 알 수 없는 전송 타입과 URL이 없는 원격 항목은 실행하지 않는다.

URL과 헤더는 stdio 설정과 같은 `${VARIABLE}` 및 `${VARIABLE:-default}`
확장을 쓴다. Authorization 헤더를 설정하면 저장된 OAuth 자격 증명보다 우선한다.
실제 자격 증명은 커밋되는
설정 파일에 넣지 않는다.

HoronAgent는 지원되는 외부 MCP 설정 소스도 읽는다. 같은
서버 이름에 로컬 stdio 정의와 원격 정의가 모두 있으면
기존 로컬 정의를 유지한다. 원격 서버를 함께 설정하려면
다른 이름을 쓴다.

### 명령

```sh
horon-agent mcp list
horon-agent mcp test remote
horon-agent mcp login remote
horon-agent mcp login remote --no-browser
horon-agent mcp logout remote
```

`test`는 설정된 서버에 연결해 프로토콜을 초기화하고 도구를 나열한 뒤
종료한다. 연결이나 핸드셰이크가 실패하면 실패로 종료한다. LLM을 호출하거나
서버의 도구를 실행하지는 않는다.

`login`은 명시적이고 대화형이다. OAuth 메타데이터를 찾고, 지원되면
공개 클라이언트를 등록하며, PKCE와 루프백 콜백으로
authorization-code 흐름을 마친다. `--no-browser`는 시스템 브라우저를 열지 않고
인증 URL을 출력한다. 그래도 브라우저는 이 기기의
루프백 콜백에 접근할 수 있어야 한다. 인증 서버가 동적 클라이언트
등록을 지원하지 않으면 Authorization 헤더를 설정한다.

토큰은 HoronAgent home 디렉터리의 `mcp-oauth.json`에 제한된
권한으로 저장된다. 토큰은 원격 리소스 범위에 묶이고 브라우저를 열지 않고
갱신된다. 백그라운드 시작과 일반 연결 시도는 대화형 로그인을
시작하지 않는다. 자격 증명이 거부되거나 없으면 사람의 응답을 기다리지 않고
해당하는 명시적 로그인 명령을 알려준다.

MCP 관리 도구는 `url`, `transport`, `headers`로 즉석 원격 연결을 받거나,
그 필드 없이 이름이 지정된 설정 서버에 연결한다. `command`, `args`, `env`는
여전히 로컬 stdio 연결 필드다.

### 시작, 연결 대기, 프롬프트 캐싱

세션 도구 등록은 원격 네트워크 트래픽을 기다리지 않는다. 관리,
검색, 디스패치 도구는 즉시 등록된다. 유효한 캐시 도구 스키마는
백그라운드 연결이 시작되기 전에 알려진다. 연결이 성공할 때마다 그 서버의 도구가
독립적으로 게시되므로, 느린 서버가 빠른 피어의 도구를 가리거나
그 피어에 대한 관리와 호출을 막을 수 없다.
비어 있는 라이브 카탈로그는 다음 시작에서 삭제된 도구를 알리는 대신
오래된 캐시 스키마를 교체하기도 한다.

원격 핸드셰이크 전체에는 30초 기한이 있다. 자격 증명
갱신, 초기화, 레거시 폴백, 도구 나열이 모두 포함된다. 개별 도구
요청도 기본 30초다. `timeout_secs`는 정당하게 느린
도구 요청을 늘릴 수 있지만 원격 핸드셰이크 전체 기한은 늘리지 않는다. 오프라인 서버에 대한 첫
호출은 연결을 최대 30초 기다린다. 취소와 종료는
대기자를 제거하고 소유한 응답 스트림을 멈춘다. 일반 도구 호출이나
시작 경로가 대화형 OAuth 로그인을 기다리는 일은 없다.

별도의 기존 headless 동작이 있다. `horon-agent run`은 첫 턴 전에
유효한 캐시 스키마가 없는 활성 서버를 최대 5초 기다린다.
웜 캐시면 이 대기를 건너뛴다. `HORON_AGENT_RUN_MCP_WAIT_MS=0`으로 끄거나 더
작은 밀리초 예산을 정한다. 비활성 서버는 예산을 쓰지 않고,
준비 판정은 정규화된 도구 이름 접두사가 아니라 정확한 서버 식별자를 쓴다.

`auto`와 `deferred` MCP 모드에서 연결, 연결 해제, 도구 발견은
즉시 등록 도구 접두사와 정적 시스템 프롬프트를 안정적으로 유지한다. provider 고유의
지연 정의는 그 접두사 밖에 있다. KV 캐시 텔레메트리는 이제 즉시 정의만 해시하고
세므로, 지연 카탈로그 변경이 하네스가 만든 캐시 미스로
잘못 나타나지 않는다. 스키마 캐시가 차갑거나 바뀐 상태의 명시적 `eager` 모드는
여전히 의도적으로 도구 스냅숏을 다시 만들어
프롬프트 캐시 미스를 일으킬 수 있다. 웜 캐시 스키마는 바뀌지 않았을 때 그 재구성을 피한다.

#### 2026-10-13에 수행한 검증

- 실제 TCP 테스트가 느린 서버의 initialize 응답을 붙잡아 두고, 그동안
  등록이 반환되고 빠른 피어가 발견 가능해지며 관리, 검색,
  빠른 도구 호출이 계속 쓸 수 있는지 단언한다. 공유 연결 경로와 소유 연결 경로를
  모두 다루며, 명시적 eager 모드의 웜 캐시 재시작도 포함한다.
- 두 서버 등록 측정값은 소유 연결이 콜드/웜 기준 357/241마이크로초,
  공유 연결이 126/219마이크로초였다
  (`justrust` 실행 `20261013-072941134-560607`). 이 값은 테스트 안의
  레지스트리 작업을 잰 것이지 전체 애플리케이션 시작이나 인터넷 연결 지연이 아니다.
- 이후 동시 CPU 부하가 심한 상태의 로컬 실행에서 소유 연결은 콜드/웜
  7.159/0.565밀리초, 공유 연결은 15.871/0.521밀리초였다
  (`20261013-074441115-730779`). 등록은 여전히
  붙잡아 둔 네트워크 응답보다 먼저 반환됐다. 서로 다른 기기와 부하
  조건은 구현 속도 향상이나 보편적 지연 상한이 아니다.
- 격리된 HoronAgent home에서 실제 네이티브 Expo OAuth 로그인이 성공했다. 네이티브
  initialize/list/shutdown 스모크 테스트가 2.404초에 도구 39개를 돌려줬다. 자격 증명이 없을 때
  같은 엔드포인트는 브라우저를 열지 않고 0.228초에 로그인 안내를
  돌려줬다.
- 회귀 테스트는 실제 에이전트의 즉시 스키마, 정적 프롬프트,
  대화, 캐시 추적기, KV 모니터를 네이티브 및 폴백 지연 provider의 HTTP 연결,
  연결 해제, 재연결 전후로 비교한다. 이는 캐시와 관련된 요청 접두사를 검증하는 것이지
  유료 provider의 캐시 적중 청구서를 검증하는 것이 아니다.
- 고정된 최종 트리의 호스티드 리플레이가 base MCP 테스트 113개, app-core MCP
  테스트 54개, 더 넓은 에이전트 테스트 79개, 루트 라이브러리 MCP CLI 테스트 11개, 실제
  stdio 충돌 통합을 통과했다. 일부 스위트는 겹친다. 앞선 로컬 검증은
  확인된 호스티드 파일 시스템 실패 뒤에 진행한 것이지
  호스티드 빌드 회계를 우회하려는 시도가 아니었다.
- 새로 빌드한 고정 바이너리는 공개 CLI HTTP/SSE/OAuth 열 건짜리
  리플레이를 통과했고 실제 Expo HTTPS MCP 서비스에서 도구 39개를 발견했다. 격리된 음성 대조에서
  안전 수정 다섯 개를 제거하자 기대한 회귀 단언이
  실패했다. 되돌리자 같은 검사를 통과했다.
  요구 사항 관찰 기록은 저장소에서 정리했고, 재현은 위 테스트로 한다.

### 수명 주기와 안전

원격 요청은 JSON-RPC 요청 ID, 협상된 프로토콜 버전,
서버가 지정한 세션 ID를 보존한다. 레거시 SSE 엔드포인트는
설정된 서버의 origin에 남아야 한다. MCP 자격 증명을 붙여 HTTP 리다이렉트를 따라가지 않는다.
종료는 대기 중 요청을 닫고 연결이 소유한 워커를 멈춘다.

인증이 필요한 원격 서버에는 HTTPS를 쓴다. 루프백 HTTP는
로컬 개발과 통합 테스트에 쓸 수 있다. OAuth 엔드포인트 발견과 콜백
검증은 보안 URL, 리소스 바인딩, PKCE, state 검증을 강제한다.

## 사용자 정의 HTTP JSON 웹 검색

JSON을 돌려주는 HTTP 엔드포인트는 무엇이든 `~/.horon-agent/config.toml`에서 설정한다.

```toml
[websearch]
engine = "custom"
prefer_native = false
fallback_engines = []

[websearch.custom]
endpoint = "https://search.example.org/api"
method = "GET"
query_param = "q"
count_param = "count"
params = { format = "json" }
headers = { Accept = "application/json" }
header_env = { Authorization = "SEARCH_AUTH" }
results_pointer = "/data/results"
title_pointer = "/title"
url_pointer = "/url"
snippet_pointer = "/snippet"
```

`prefer_native`의 기본값은 true다. 지원하는 모델 제공자가 로컬 사용자 정의 엔진을
제공자 네이티브 검색으로 바꾸지 않게 하려면 **false**로 둔다. 기존 DuckDuckGo와 Bing
기본값은 그대로다. 오류가 나거나 결과가 비면 `fallback_engines`를 순서대로 시도한다.
기본 대체 엔진은 Bing이다. 비공개 질의를 다른 곳으로 보내면 안 되면 `[]`로 둔다. 사용자
정의 엔진을 대체 엔진으로 쓸 수도 있다.

### 요청

`endpoint`는 사용자 정보와 프래그먼트가 없는 절대 HTTP(S) URL이어야 한다.
원격 서비스에는 HTTPS를 쓴다. GET과 POST만 지원하며 설정에서 대소문자를 가리지 않는다.
기본은 GET이다. `params`는 두 방식 모두에 적용되는 정적 URL 질의 매개변수이고, 같은
이름의 엔드포인트 질의 매개변수를 덮어쓴다. GET에서는 질의와 선택적인 개수가 URL
매개변수가 된다. 동적 값은 정적 값과 같은 이름의 엔드포인트 매개변수를 덮어쓴다. 모든
매개변수는 URL 인코딩한다.

POST에서 `body`는 정적 JSON 객체다(기본값 `{}`). 질의는 `query_param` 자리에 문자열로
들어가고, 개수는 `count_param` 자리에 JSON 정수로 들어간다. 이 둘은 포인터가 아니라 본문
최상위 키 그대로이며, 이미 있는 본문 키를 덮어쓴다. 예를 들면 다음과 같다.

```toml
[websearch.custom]
endpoint = "https://search.example.org/api"
method = "POST"
query_param = "search"
count_param = "limit"
params = { version = "1" }
body = { options = { safe = true } }
```

`query_param`의 기본값은 `q`다. 개수 매개변수를 보내지 않으려면 `count_param`을 생략한다.
넣을 때는 비어 있지 않아야 하고 `query_param`과 달라야 한다. 결과는 여전히 `num_results`로
로컬에서 제한한다(도구 기본값 8, 최대 20).

`headers`는 정적 헤더 값을 넣는다. 자격 증명에는 `header_env`를 쓰는 편이 좋다. 헤더
이름을 환경변수 이름에 대응시키고, 정적 헤더를 대소문자 구분 없이 덮어쓴다. 환경변수에는
필요한 경우 `Bearer `를 포함한 **전체** 값이 들어간다. 변수가 없거나 비어 있으면, 또는
헤더 값이 잘못되면 네트워크 접근 전에 실패한다. 자격 증명은 진단 정보에 복사하지 않는다.
헤더 인증을 쓸 수 있으면 URL이나 본문에 자격 증명을 넣지 않는다.

사용자 정의 요청의 제한 시간은 30초이고, 자격 증명이 다른 목적지로 전달되지 않도록 같은
호스트 리다이렉트를 포함해 리다이렉트를 따라가지 않는다. 요청 오류에서는 reqwest URL을
지운다. HTTP 오류는 상태만 보여주고 응답 본문, 설정한 URL, 헤더는 보여주지 않는다. 이런
보호 장치는 설정한 엔드포인트를 신뢰하는 일을 대신하지 않는다. 평문 HTTP는 자격 증명을
암호화하지 않고 보낸다.

### JSON 결과 매핑

포인터는 JSON Pointer(RFC 6901)를 따른다. `/`는 `~1`로, `~`는 `~0`으로 이스케이프한다.
`results_pointer`의 기본값은 `/results`이고 배열로 해석되어야 한다. 최상위 배열이면 `""`를
쓴다. 제목, URL, 스니펫 포인터는 각 배열 항목 기준 상대 경로이고 기본값은 `/title`,
`/url`, `/snippet`이다. `/link/href`처럼 중첩 경로도 지원한다. URL이 없거나 문자열이
아니거나 비어 있으면 그 항목을 건너뛴다. 제목이 없거나 문자열이 아니거나 비면 URL을 대신
쓰고, 스니펫이 없거나 문자열이 아니면 빈 값이 된다. JSON이 깨졌거나 포인터 문법이
틀렸거나 결과 매핑이 없거나 배열이 아니면 분명하게 실패하고 설정한 대체 엔진을 쓴다.
유효한 빈 배열은 결과 없음이다.

## 안내식 Google 설정 (직접 만든 OAuth 앱)

상태: 구현됨. CLI 마법사(`horon-agent login google`)와 아래의 에이전트 주도
브라우저 흐름은 같은
헬퍼(`crates/base/src/auth/google/setup.rs`)를 공유한다.

### 결정

모든 사용자는 `direct` 백엔드에서 **자기 Google Cloud OAuth 앱**으로
horon-agent의 Google 통합(Gmail, Calendar, 나중에 Drive·Docs)을 승인한다.
horon-agent는 공유 Google OAuth 클라이언트를 배포하지 않으며, 기본값으로
Google 데이터를 제3자를 거쳐 보내지 않는다.

horon-agent는 설정 부담을 두 가지 방식으로 덜어 준다.

1. **CLI 마법사** (`horon-agent login google`, 다시 하려면 `--setup`).
   `gcloud`가 설치되어 있고 로그인되어 있으면 그것으로 프로젝트를 만들고
   모든 API를 켜며, 남은 콘솔 페이지를 프로젝트가 미리 선택된 상태로 열고,
   OAuth 클라이언트 JSON이 다운로드 폴더에 떨어지면 자동으로 가져온다.
2. **에이전트 주도 설정.** Google 도구가 설정되지 않았다고 보고하면
   에이전트가 브라우저 도구로 사용자의 브라우저에서 Google Cloud 콘솔을
   조작하고, 다운로드 파일을 `--google-client-json auto`로 horon-agent에
   넘긴다.

#### 다른 선택지를 쓰지 않는 이유

| 선택지 | 문제 |
|---|---|
| horon-agent가 소유한 공유 검증 앱 | Gmail 스코프(`gmail.readonly`, `gmail.modify`, `gmail.compose`)는 *제한* 등급이다. Google 검증에 더해 매년 CASA 평가를 받아야 한다(몇 주의 심사, 반복 비용). Calendar만은 *민감* 등급(표준 검증)이라 나중에 Calendar용 공유 앱은 선택지로 남는다. |
| Google의 Workspace MCP 서버 | 개발자 프리뷰이고, 여전히 직접 OAuth 앱을 만들어야 하며 제품마다 MCP API가 하나씩 더 필요하다. Gmail은 읽기·초안·라벨만 된다(발송, 휴지통, 초안 편집 불가). |
| Composio(`HORON_AGENT_GMAIL_BACKEND=composio`) | 모든 요청과 그 페이로드가 Composio 프록시를 거치고, Composio가 모든 사용자의 Google 리프레시 토큰을 보관한다. 선택 기능으로만 남긴다. |
| `gog` CLI | 설치가 하나 더 필요하고 여전히 직접 앱을 만들어야 하며, 파일 키링은 비대화형 에이전트 셸에서 `GOG_KEYRING_PASSWORD`를 요구한다. 이 도구의 `gog auth setup`이 마법사의 본보기가 되었다. |

#### 직접 만든 앱이 통하는 이유

- 앱 하나에 사용자 하나이므로 테스트 사용자 100명 상한이 적용되지 않는다.
- 검증도 CASA도 없다. 사용자가 자기 앱을 승인하기 때문이다.
- 트래픽은 기기에서 Google로만 간다. 토큰은
  `~/.horon-agent/google_oauth.json` (0600)에 있다.
- 서비스를 추가하는 데 심사가 필요 없다. API를 켜고 다시 동의하면 끝이다.

피할 수 없는 화면이 하나 있다. 로그인할 때 Google이 띄우는 **"Google hasn't
verified this app"** 경고다. 직접 만든 앱은 개인 Gmail 계정에서 모두 이
경고를 보여 준다. Workspace 계정은 내부(Internal) 대상으로 두면 이 경고를
피할 수 있다.

### 에이전트 플레이북

`gmail`이나 `calendar`가 설정되지 않았다고 말하거나 사용자가 Google 설정을
요청할 때 이 절차를 쓴다. 사용자가 이미 Google에 로그인되어 있는 평소
브라우저 프로필에서 브라우저 도구로 단계를 진행한다. 비밀번호나 2단계 인증
코드는 절대 입력하지 않는다.

1. **서비스 고르기.** 기본값은 전체(`gmail,calendar`)다. 사용자가 콘솔을 한
   번만 거치게 하려는 것이다.
2. **프로젝트와 API.**
   `gcloud auth list --filter=status:ACTIVE --format="value(account)"`가
   계정을 출력하면(`config get-value account`로는 부족하다. 자격 증명이 없는
   계정도 보고한다) 다음을 실행한다.
   ```
   gcloud projects create horon-agent-<6 digits> --name horon-agent --quiet
   gcloud services enable gmail.googleapis.com calendar-json.googleapis.com --project <id> --quiet
   ```
   그렇지 않으면 `https://console.cloud.google.com/projectcreate`를 열어 `horon-agent`라는 이름의 프로젝트를 만들고, 모든 API를 한 번에 켜는 링크 `https://console.cloud.google.com/flows/enableapi?apiid=gmail.googleapis.com,calendar-json.googleapis.com&project=<id>`를 연다.
3. **동의 화면.**
   `https://console.cloud.google.com/auth/branding?project=<id>`를 연다.
   "Google Auth Platform not configured yet"이라고 나오면 Get Started를
   누른다. 앱 이름 `horon-agent`, 지원·연락 이메일은 사용자 이메일, 대상은
   외부(External)다(허용하는 Workspace 계정은 내부). 동의하고 Create를
   누른다. 스코프는 추가하지 않는다. horon-agent가 로그인할 때 요청한다.
4. **앱 게시.**
   `https://console.cloud.google.com/auth/audience?project=<id>`에서
   "Publish app"을 누르고 확인한다. 필요한 이유는 이것이다. 테스트
   모드에서는 Google이 로그인을 **7일** 뒤에 만료시킨다. 자기 앱을 게시하는
   데는 심사가 필요 없다.
5. **OAuth 클라이언트 만들기.**
   `https://console.cloud.google.com/auth/clients/create?project=<id>`에서
   유형 **Desktop app**, 이름 `horon-agent`로 Create를 누른 뒤 **Download
   JSON**을 누른다. 데스크톱 클라이언트는 URI를 등록하지 않아도
   horon-agent의 루프백 리다이렉트를 받아 준다.
6. **가져오기와 로그인.**
   ```
   horon-agent login google --google-client-json auto --google-services gmail,calendar
   ```
   `auto`는 다운로드 폴더에서 가장 최근의 유효한 `client_secret*.json`을 고른다. 시크릿은 파일에서 `~/.horon-agent/google_credentials.json`으로 바로 가고 대화 기록에는 절대 들어가지 않는다. 사용자가 로그인하기 전에 미검증 앱 경고(Advanced, 그다음 Go to horon-agent)를 알려 주고 모든 체크박스를 그대로 두라고 말한다.
7. **확인.** 승인된 서비스마다 읽기 호출을 한 번씩 한다(최근 메일 나열,
   다가오는 일정 나열). 그다음 어느 서비스가 활성인지 보고한다.

### 실패 처리

| 증상 | 원인 | 조치 |
|---|---|---|
| Workspace 계정에서 `access_denied` / "app is blocked" | 관리자가 미검증 제3자 앱을 차단한다 | 관리자에게 클라이언트 ID 허용을 요청하거나 개인 계정을 쓴다. 재시도하지 않는다. |
| `accessNotConfigured` / "has not been used in project" | API가 꺼져 있다 | 2단계의 활성화 링크를 다시 열고 1분쯤 기다린 뒤 재시도한다. |
| `No refresh token received` | 오프라인 동의 없이 이전 승인이 남아 있다 | `https://myaccount.google.com/permissions`에서 취소하고 다시 로그인한다. |
| 잘 되다가 일주일쯤 뒤 `invalid_grant`로 실패 | 앱이 테스트 모드에 남아 있다 | 앱을 게시하고(4단계) 다시 로그인한다. horon-agent의 리프레시 오류 메시지도 같은 내용을 알려 준다. |
| 가져오기가 "Web application client"라고 한다 | 클라이언트 유형이 틀렸다 | Desktop app 클라이언트를 만든다(5단계). |
| 콘솔 UI가 바뀌었거나 선택자가 실패한다 | Google Cloud UI 변경 | `horon-agent login google --setup`으로 되돌아간다. 각 페이지를 열어 준다. |

### 나중에 서비스 추가하기

`horon-agent login google --google-services <existing>,<new>`를 다시
실행한다. 로그인은 이미 승인된 서비스를 유지하고 스코프 합집합을
요청한다(`include_granted_scopes=true`). 새 서비스의 API를 먼저 켜 둔다.

### 남은 작업

- 데스크톱: 데스크톱 전용 구현이 아니라 SDK를 거쳐 데스크톱 설정의 Accounts
  페이지에서 같은 흐름을 제공한다.
- 브라우저 도구 신뢰성: 콘솔 흐름은 길고 여러 페이지에 걸치므로 에이전트
  경로는 안정적인 브라우저 핸드오프에 기댄다.
