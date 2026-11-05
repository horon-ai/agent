# 도구 스케줄링과 루프 가드

메시지의 도구 호출이 어떻게 실행되는지를 두 가지 동작이 결정합니다. 스케줄러는
모델 메시지 하나의 호출을 묶고 읽기 전용 호출을 나란히 실행합니다. 루프 가드는
도구 호출 순서에서 반복을 지켜봅니다. 이 문서는 두 동작이 턴 루프에 연결된
현재 트리의 동작을 설명합니다.

## 라운드 묶기

모델 메시지 하나가 도구 호출 여럿을 담을 수 있습니다. 그 라운드는 요청
순서대로 그룹으로 쪼개지고, 각 그룹은 한 단위로 실행됩니다.

- 호출은 자신이 무엇을 건드리는지 효과(effect) 비트마스크로 선언합니다. 읽기와
  네트워크는 서로 겹쳐도 되고, 쓰기·덧붙이기·프로세스는 장벽입니다.
- 열려 있는 그룹의 누적 효과와 새 호출의 효과가 `compatible_with`를 만족하는
  동안 그 호출은 같은 그룹에 들어갑니다.
- 만족하지 않는 호출은 열려 있는 그룹을 닫고 새 그룹을 엽니다. 장벽 효과를
  선언한 호출은 그래서 어떤 것과도 겹치지 않습니다.
- 레지스트리에 없는 이름의 호출은 `ToolEffects::write()`로 취급합니다. 효과를
  모르는 호출은 절대 겹쳐 실행하면 안 됩니다.

이 분할은 호출별 효과 목록에 대한 순수 함수이므로
(`crates/app-core/src/agent/tool_round.rs:134` `plan_groups`), 에이전트를 띄우지 않고도 묶기를 테스트할 수 있습니다. 효과는
호출마다 `Tool::effects_for_input`으로 읽으므로, 같은 도구라도 입력에 따라
네트워크 요청을 쓰는 호출만 세어집니다 (`tool_round.rs:182` `classify_call`). 세션 범위 SDK 사용자 정의
도구는 이름이 내장 별칭과 같아도 장벽으로 강제됩니다. 그 호출은 같은 이름의
레지스트리 도구가 아니라 콜백을 실행하기 때문입니다.

두 턴 루프가 같은 분할기를 부릅니다. 비스트리밍 경로는
`plan_tool_round_groups`를 쓰고
(`crates/app-core/src/agent/turn_loops.rs:53` `plan_tool_round_groups`), 스트리밍 데몬 경로도 같은 헬퍼를 부르므로
(`crates/app-core/src/agent/turn_streaming_mpsc.rs:1668` `plan_tool_round_groups`), 묶기 규칙과 상한, 순서 규칙이 둘 사이에서 어긋날 수 없습니다.

## 어떤 도구가 겹칠 수 있는가

효과는 도구 자신이 정합니다. `Tool::effects`의 기본값은 장벽인
`ToolEffects::write()`이고(`crates/tool-core/src/lib.rs:255` `Tool::effects`),
입력에 따라 달라지는 도구는 `Tool::effects_for_input`을 덮어씁니다
(`crates/tool-core/src/lib.rs:260` `Tool::effects_for_input`). 동시성 등급은 이 효과에서
파생되므로(`crates/tool-core/src/lib.rs:270` `Tool::concurrency_class`), 스케줄러는
등급을 거치지 않고 효과를 직접 씁니다. 기본값이 장벽인 것은 의도적입니다.
겹치는 두 호출이 같은 파일에 쓰면 사용자 데이터를 잃을 수 있지만, 불필요한
장벽은 속도만 잃습니다.

효과를 선언하는 도구는 다음과 같습니다.

| 도구 | 출처 |
| --- | --- |
| `read` | `crates/app-core/src/tool/read.rs:131` `effects` |
| `read` (원격 경로) | `crates/app-core/src/tool/read.rs:135` `effects_for_input` |
| `ls` | `crates/app-core/src/tool/ls.rs:56` `effects` |
| `agentgrep` | `crates/app-core/src/tool/agentgrep.rs:190` `effects` |
| `webfetch` | `crates/app-core/src/tool/webfetch.rs:51` `effects` |
| `websearch` | `crates/app-core/src/tool/websearch.rs:56` `effects` |
| `bash` | `crates/app-core/src/tool/bash.rs:829` `effects` |

나머지는 기본값인 장벽에 남는데, `write`, `edit`, `replace`, `apply_patch`,
`batch`, `mcp`, `skill_manage`가 그렇습니다. 레지스트리
조회는 제공자 별칭을 해석하고, 자기에게 없는 이름에는 `ToolEffects::write()`로 답합니다 (`crates/app-core/src/tool/mod.rs:1038` `Registry::effects_for_call`).

## 그룹당 상한 두 개

그룹은 두 상한 중 하나에 닿으면 멈추고, 두 상한은 서로 독립입니다 (`crates/app-core/src/agent/tool_round.rs ~:12-18`).

| 상한 | 값 | 출처 |
| --- | --- | --- |
| 그룹 하나의 구성원 | 10 | `tool_round.rs:45` `MAX_GROUP_MEMBERS` |
| 그룹 하나의 네트워크 구성원 | 4 | `tool_round.rs:52` `MAX_NETWORK_MEMBERS` |

네트워크 구성원은 효과에 `NETWORK` 비트가 있는 호출입니다. 도구 이름 목록은
쓰지 않으므로, 입력에 따라 네트워크 요청을 쓰는 호출(원격 경로를 읽는 `read`)도
그 호출만 세어집니다. 상한은 그룹이 열려 있는 동안 누적 효과와 누적 개수로
확인합니다 (`tool_round.rs:143` `active.compatible_with`). 그래서 섞인 그룹은 로컬 읽기가 아직
들어갈 자리가 있어도 네트워크 호출 네 건에서 멈춥니다. 네트워크 호출마다 유료
API 요청을 쓰기 때문입니다.

## 결과는 요청 순서대로 도착합니다

그룹 구성원은 동시에 돌고 끝나는 순서대로 끝나지만, 결과는 라운드 안에서의
인덱스로 적용됩니다. `run_group`은 모은 결과를 반환 전에 인덱스로 정렬하고
(`tool_round.rs:348` `results.sort_by_key`), 두 루프 모두 그룹 구성원을 요청
순서로 적용하므로 터미널 이벤트, 저장된 소요 시간, 이력 메시지가 모델의 순서를
따릅니다(`turn_loops.rs:1543` `results.remove`,
`turn_streaming_mpsc.rs:894` `results.insert`). 제공자는 각 `tool_use`와 그
`tool_result`를 id로 짝짓기 때문에, 순서가 뒤바뀐 라운드는 트랜스크립트를
망칩니다.

그룹 안에서는 실패 규칙 두 가지가 성립합니다.

- 각 구성원은 자기 태스크에서 돌므로, 패닉하는 도구는 그룹 전체를 잃는 대신 그
  호출의 오류 결과가 됩니다(`tool_round.rs:282` `tokio::spawn`).
- 취소되면 그룹은 이미 가진 결과를 유지하고 결과를 내지 못한 인덱스를
  보고합니다. 그래야 호출자가 그 인덱스마다 건너뛴 결과를 쓰고
  `tool_result`를 `tool_use`마다 하나씩 유지할 수 있습니다 (`tool_round.rs:354` `unstarted`).

## 컨텍스트 예산

실행된 도구는 출력을 도구 계층의 넘침 가드에 통과시킵니다
(`crates/app-core/src/tool/mod.rs:1415` `self.guard_context_overflow`, `guard_context_overflow` 안의 검사, `tool/mod.rs:1415` `guard_context_overflow`).
그 가드는 출력을 `token_budget()` 곱하기 `CONTEXT_GUARD_THRESHOLD`와
비교합니다(0.90, `tool/mod.rs:1048` `CONTEXT_GUARD_THRESHOLD`, 비교는 `tool/mod.rs:1461` `needs_truncation`).

그 가드는 자기가 들고 있는 컴팩션 관리자를 읽습니다. `Registry::clone`은 그
관리자를 새것으로 바꾸는 반면 `clone_sharing_compaction`은 부모의 관리자를
공유합니다(`tool/mod.rs:511` `CompactionManager::new`,
`tool/mod.rs:531` `clone_sharing_compaction`). 그래서 가드는 새 관리자가 보고할
바닥값이 아니라 실제 세션 예산과 토큰 수를 봅니다. 두 루프 모두 도구를 실행하는
라운드에 공유 클론을 넘깁니다(`turn_loops.rs:1487` `run_group`, `turn_streaming_mpsc.rs:1862` `clone_sharing_compaction`, `turn_streaming_mpsc.rs:1658` `run_group`).

라운드는 자기 카운터도 하나 가집니다. 도구 계층 가드가 자기가 속한 라운드를 볼
수 없기 때문입니다. `effective_token_count()`는
`max(token_estimate, observed)`로 계산하고
(`crates/base/src/compaction/mod.rs:1554` `effective_token_count`), 추정값은
메시지 0개로 잡으며(`crates/base/src/compaction/mod.rs:1535` `token_estimate`) `observed_input_tokens`는
제공자 응답이 돌아온 뒤에만 움직입니다
(`crates/base/src/compaction/mod.rs:1540` `update_observed_input_tokens`). 그래서 라운드 중간에 그 카운터를 다시 읽어도 같은 라운드에서 앞서 적용된 출력은
보이지 않습니다. 출력 여럿이 각각 출력별 가드를 통과하고도 합쳐서 윈도를 넘칠 수
있습니다.

`RoundTokenBudget`이 그 틈을 메웁니다. 라운드는 에이전트 자체 컴팩션 관리자와
세션 메시지로 카운터 하나를 시드하고
(`crates/app-core/src/agent/round_budget.rs:62` `RoundTokenBudget::seed`), 모델에 돌려주는 것의 토큰만큼 전진합니다. 허용한 출력, 대신 넣는 거부, 잘린
텍스트가 그 대상입니다(`round_budget.rs:142` `RoundTokenBudget::admit`). 두 루프
모두 라운드마다 한 번 만들어 모든 그룹에 넘기므로, 뒤 그룹은 앞 그룹이 이미 쓴
만큼을 청구받습니다(`turn_loops.rs:1501` `RoundTokenBudget::seed`,
`turn_streaming_mpsc.rs:1677` `RoundTokenBudget::seed`). 라운드는 도구 계층의
규칙을 자기 누계에 적용합니다(`round_budget.rs:160` `projected_tokens`). 들어가는
출력은 허용하고, 들어가지 않는 출력은 호출자가 지불을 요청하지 않았으면
보류하고 요청했으면 자릅니다(`round_budget.rs:171` `single_output_max_tokens`).

상한값, 잘림 안내, 윈도 소진 거부와 과대 거부는 복사본이 아니라 도구 계층의
것을 그대로 씁니다. `Registry::context_guard_threshold_tokens`
(`tool/mod.rs:1053` `context_guard_threshold_tokens`)가 예산을 0.90 상한으로
바꾸고, `Registry::truncated_output_notice`
(`tool/mod.rs:1086` `truncated_output_notice`)가 잘린 앞부분 뒤에 붙는 문장을
씁니다. 라운드는 출력별 상한을 도구 계층에 맡기지 않고 두 상한을 직접
강제합니다. `admit`은 예상 합계가 들어가고 이 출력이 단일 출력 상한 안에 있을
때만 출력을 돌려줍니다(`round_budget.rs:178` `projected_tokens`). 그래서 도구
계층이 거부할 과대 출력은 여기서도 거부되고, 누적 경우가 그 위에 더해집니다.
구성원마다 혼자서는 통과했지만 합이 들어가지 않는 경우입니다.

라운드는 경우 셋이 다르므로 서로 다른 문장 셋으로 거부합니다. 윈도가 이미 다 쓴
상태면(`current >= threshold`) 자리가 없으므로 라운드는
`Registry::exhausted_context_refusal` (`tool/mod.rs:1069` `exhausted_context_refusal`)을 라운드의 누계와 함께 다시
씁니다. 그 문장은 `accept_large_output`이 도움이 될 수 없다고 말하며, 그
상황에서는 맞습니다. 자리는 남았는데 이 출력이 통째로 들어가지 않으면
`ceiling_bound`(`round_budget.rs:211` `ceiling_bound`)가 어느 문장이 진짜 이유를
말할지 고릅니다. 단일 출력 상한이 둘 중 더 엄격하면
(`single_max_tokens < remaining_tokens`) 라운드는 도구 계층의
`Registry::oversized_output_refusal` (`tool/mod.rs:1242` `oversized_output_refusal`)을 출력합니다. 그래서 호출자는
어느 계층이 거부했는지 알 수 없습니다. 아니면 라운드는 자기
`withheld_output_refusal`(`round_budget.rs:290` `withheld_output_refusal`)을
씁니다. 같은 "CONTEXT LIMIT REACHED" 접두사와 `/compact` 처방이지만,
`accept_large_output` 탈출구와 그래도 들어갈 토큰 수를 적습니다. 그 플래그는 다음
라운드에서 앞부분을 사 주기 때문입니다. 이미 플래그를 넘긴 호출자는 이 지점에서
거부되지 않고, 출력은 두 상한 중 더 엄격한 쪽으로 잘립니다 (`round_budget.rs:171` `single_output_max_tokens`).

## 루프 가드

가드는 현재 턴의 최근 호출을 롤링 윈도로 유지하고(64개 기록,
`crates/app-core/src/agent/loop_guard.rs:84` `WINDOW_CAPACITY`), 디스패치 결정마다 질문 하나에 답합니다.

상승 사다리는 하나이고 한 곳에 적혀 있습니다 (`loop_guard.rs:301` `LoopGuard::record_and_decide`).

| 실행 길이 | 판정 | 출처 |
| --- | --- | --- |
| 같은 실행 3회 | 경고한 뒤 호출 실행 | `loop_guard.rs:87` `IDENTICAL_WARN_AT`, `loop_guard.rs:344` `identical_warning_text` |
| 같은 실행 4회 이상 | 거부하고 호출을 실행하지 않음 | `loop_guard.rs:90` `IDENTICAL_REFUSE_AT`, `loop_guard.rs:341` `refusal_text` |
| 한 서명의 한 도구가 거부 2회 | 정지하고 턴 종료 | `loop_guard.rs:97` `STOP_AFTER_REFUSALS`, `loop_guard.rs:339` `stop_text` |
| 순환 첫 감지 | 경고한 뒤 호출 실행 | `loop_guard.rs:367` `cycle_warning_text` |
| 같은 순환 재감지 | 거부하고 호출을 실행하지 않음 | `loop_guard.rs:379` `cycle_refusal_text` |
| 같은 순환의 거부 2회 | 정지하고 턴 종료 | `loop_guard.rs:377` `cycle_stop_text` |

실행(run)은 같은 도구에 같은 입력입니다. `identical_run`은 도구 이름과 서명을
함께 비교하고(`loop_guard.rs:455` `identical_run`), 거부 카운터는 둘을 키로
씁니다(`loop_guard.rs:339` `refusals.entry`). 그래서 한 경로에서 `read`,
그다음 `lsp_diagnostics`, 그다음 `read`는 호출 셋이고 실행 하나가 아닙니다. 한
도구의 거부가 다른 도구의 사다리를 올리지도 않습니다.

거부된 호출도 기록하므로, 거부 뒤에 같은 입력을 반복하면 사다리가 처음으로
돌아가지 않고 올라갑니다(`loop_guard.rs:301` `LoopGuard::record_and_decide`).
비교는 정규화된 서명으로 합니다. 객체 키를 정렬하고 `intent` 필드를 버리므로,
적어 낸 이유만 다른 두 호출은 같게 셉니다(`loop_guard.rs:455` `identical_run`,
`intent` 건너뛰기는 `loop_guard.rs:531` `skip_intent`).

가드는 디스패치 결정마다 한 번만 조회하고, 실행 중인 그룹 안에서는 절대 조회하지
않습니다(`crates/app-core/src/agent/loop_guard.rs ~:8-16`). 그룹 하나가
결정 하나입니다. 도구에 닿을 구성원은 모두 시작 전에 기록되므로, 같은 `read`
호출 네 건을 담은 그룹은 그 결정에서 길이 4에 닿고 네 번째 구성원이 그 자리에서
거부되며, 처음 셋은 그대로 실행됩니다. 잘못된 호출과 SDK가 답한 호출은 도구에
닿지 않으므로 기록되지 않고 판정되지도 않습니다. 디스패치 경계는 호출자가
소유하고, 모듈은 동시성에 대해 아무것도 모릅니다.

같은 실행과 순환은 같은 사다리를 오릅니다. 순환 감지기는 2~6개 호출 주기가 세
번 반복될 때 작동하고(`loop_guard.rs:468` `cycle_run`), 첫 감지는 경고만
합니다(`loop_guard.rs:367` `cycle_warning_text`). 그 뒤로 같은 순환은 거부되고
다음에 정지합니다(`loop_guard.rs:379` `cycle_refusal_text`). 영원히 반복되는
경고는 정지가 아니며, `read a.rs -> bash -> read a.rs`에 갇힌 데몬은 그대로 두면
디스패치마다 알림 하나만 쌓고 끝나지 않을 것입니다. 순환의 정체성은 구성 도구
이름과 서명을 사전순으로 가장 작은 회전으로 줄인 것이므로
(`loop_guard.rs:423` `cycle_key`), `read -> bash`와 `bash -> read`는 하나의
루프이고 위상 회전이 사다리를 다시 시작시키지 않습니다. 사다리 상태는
`Option<CycleLadder>` 슬롯 하나에 살고(`loop_guard.rs:262` `cycle_ladder`, 구조체는
`loop_guard.rs:275` `CycleLadder`), 꼬리가 더 이상 그 순환이 아니게 되는 즉시
버려지므로(`loop_guard.rs:327` `cycle_ladder`), 한 단계의 입력을 바꾸면 거부가
풀립니다.

순환 감지기는 구성원이 모두 서로 다른 정체성을 겨냥한 회전을 건너뛰므로, 다른
파일을 번갈아 다루는 작업은 경고하지 않습니다
(`loop_guard.rs:492` `every_member_has_a_target`). 이 예외는 들리는 것보다
좁습니다. `target_identity`는 `read`, `lsp_diagnostics`, `bash_output`,
`task_output`, `bg`에만 있고(`loop_guard.rs:568` `target_identity`), 다른 모든
도구는 `None`으로 답하므로 `every_member_has_a_target`이 거짓이고 순환은 예외가
되지 않습니다. 그래서 `ls`, `webfetch`, `agentgrep`을 두 대상 사이에서 세 번
순환시키면 실제로 작동합니다. 집합을 좁게 유지하는 것은 의도입니다. 입력에서
대상을 읽을 수 없는 도구야말로 그 도구에 대한 실제 두 대상 루프의 모습이므로,
예외를 넓히면 그 루프에 대한 가드가 약해집니다. "비슷함" 단계는 없습니다. 최신
호출과 인자만 다른 실행은 평범한 작업이고(다음 페이지, 다음 파일), 서명까지 맞는
실행은 이미 같은 실행입니다. 모듈에 유사도 점수가 없는 이유입니다 (`crates/app-core/src/agent/loop_guard.rs ~:55-59`).

### 폴링은 거부 단을 건너뛰고, 대기에는 상한이 있습니다

한 부류의 호출은 입력이 정당하게 일정합니다. 스스로 변하는 자원을 읽는
경우입니다. `bg`의 상태, 대기, 출력, 꼬리, 목록 동작과 `bash_output`,
`task_output` 리더, 그리고 `mcp_call`(또는 네이티브 `mcp__<server>__<tool>` 호출)로
닿는 상태형 원격 도구는 매번 같은 방식으로 묻고 답이 변합니다. 그런 호출은
기록마다 한 번 분류하고(`loop_guard.rs:614` `polling_call`), 같은 실행 사다리는
그중 하나도 거부하지 않습니다. 같은 폴링의 실행은 경고 단계에서 알림을 정확히
하나 남기고(`loop_guard.rs:685` `polling_warning_text`, `loop_guard.rs:660` `poll_verdict`),
그 실행의 이후 호출은 아래에 설명하는 별도 상한까지 모두 실행됩니다. 구성원이
전부 폴링인 순환도 같은 방식으로 다룹니다
(`loop_guard.rs:691` `polling_cycle_warning_text`, `loop_guard.rs:378` `LoopGuardVerdict::Allow`). 이런 호출을 거부하면 자기가 물어본 바로 그 결과를 기다리는 턴이 끝나므로,
사다리는 절대 거부하지 않습니다.

대기는 사다리가 아니라 두 번째 그물로 제한합니다. 연속 폴링은 상한 24로 세고
(`loop_guard.rs:109` `POLLING_CALL_CAP`), 세는 단위는 일부러 거칩니다. 같은
폴링이 아니라 폴링을 세는데, 상태 호출마다 필드 하나를 바꿔도 요청 한 건을
온전히 쓰기 때문입니다. 한 실행의 처음 24번 폴링은 실행되고, 25번째는
디스패치되지 않으며 턴이 거기서 끝납니다(`loop_guard.rs:275` `consecutive_polls`,
`loop_guard.rs:673` `poll_cap_stop_text`). 상한은 호출을 거부하지 않습니다. 25번째
폴링에서 턴을 정지하고, 같은 실행 사다리와 전부 폴링인 순환도 거부하지 않습니다.
(폴링이 아닌 호출을 포함한 순환의 구성원인 폴링은 그 순환의 일부로 거부될 수
있으며, 이는 평범한 사다리가 동작하는 것입니다.) 정지된 호출은 실행되지 않고 그
도구 결과에 정지 안내가 담깁니다. 라운드에서 그 뒤의 모든 호출은 오류 표시가
붙은 `[Skipped: the loop guard ended the turn]` 결과로 쓰여 트랜스크립트가 균형을
유지합니다(`turn_streaming_mpsc.rs:2263` `LOOP_GUARD_SKIP`). 실제로 실행된 폴링
아닌 호출은 실행을 끝내고, 턴 경계가 카운트를 지우므로
(`loop_guard.rs:293` `consecutive_polls`) 다음 턴은 온전한 허용량으로 시작합니다.
잘못된 호출과 SDK가 답한 호출은 가드에 아예 닿지 않습니다. 두 루프 모두 조회
전에 디스패치 집합에서 빼기 때문이며
(`turn_loops.rs:1567` `RoundMember::Execute`, `turn_streaming_mpsc.rs:1818` `consult_loop_guard`), 둘 다 실행을 끊지 않습니다.

스트리밍 데몬 경로에서는 정지를 `TurnStopReason::LimitReached`로 알립니다
(`turn_streaming_mpsc.rs:55` `TurnStopReason::LimitReached`). 와이어 프로토콜이
"어떤 한도가 이 턴을 끝냈다"는 뜻으로 이미 나르는 사유입니다(비스트리밍 경로는
같은 안내를 기록하고 정지 사유 없이 돌아옵니다). 하네스 안의 어느 것도 그 턴을
다시 시작하지 않습니다. 정지는 턴을 끝내고 다음 수를 세션 소유자에게 남깁니다.
SDK의 재시도 정책은 그 사유에서 포기하고
(`crates/agent-sdk/src/turn_retry.rs:407` `TurnStopReason::LimitReached`), 재시도는 오류 이벤트에서만 예약되는데 루프 가드 정지는 그런 이벤트를 내지
않습니다(`crates/agent-sdk/src/turn_retry.rs:393` `observe_error`). 그래서
안내는 세션을 소유한 클라이언트나 데몬이 다음 턴을 시작해야 한다고 분명히
말하고, 정지된 턴 안의 어떤 것도 재개를 기대할 수 없습니다. 새 턴이 얻는 것은
새 허용량입니다. 턴 진입마다 가드가 초기화되기 때문입니다. `/overnight`
감독자(`overnight.rs:374` `run_turn_monitored`)는 정지된 턴을 완료된 턴으로 보고
이어가기 프롬프트에서 다음 턴을 시작하며
(`overnight.rs:368` `build_continuation_prompt`), 그 턴은 `run_turn`에
`run_once_capture`를 거쳐 닿고(`turn_execution.rs:108` `run_turn`), 진입 시 가드를
초기화합니다(`turn_loops.rs:426` `loop_guard.reset`). 그래서 상한은 실행이 아니라
턴을 제한합니다. 그런 감독자가 없는 무인 데몬에서는 운영자가 하나를 시작할 때까지
대기가 고아가 됩니다. 이것은 의도한 맞바꿈입니다. 운영자가 볼 수 있는 유한한
정지가 아무도 못 보는 무한 루프보다 낫습니다.

분류는 일부러 좁습니다. 그래야 예외가 실제 작업의 구멍이 되지 않습니다. `bg`는
위에 든 동작만 셉니다. `cancel`, `cleanup`, `watch`, `delivery`, `subscribe`는
상태를 바꾸므로 사다리를 그대로 유지합니다. 마지막 셋은 모두 도구의 같은 갈래를
타는데, 그 갈래는 호출마다 `update_delivery`로 작업의 전달 설정을 다시 쓰고
`stall_wake_seconds`가 주어지면 정체 감시 장치를 다시 무장하므로, 하나를 반복하는
것은 대기가 아니라 쓰기 루프입니다. 원격 도구는 이름에 상태 단어가 있고 변경
단어가 없을 때 폴링이므로, `job_status`와 `get_job`은 폴링이고 `write_file`,
`create_job`, `delete_job`은 아닙니다(`loop_guard.rs:641` `names_a_poll`). `read`,
`ls`, `agentgrep`, `bash`, `write`, `edit`을 포함한 나머지는 이 규칙의 영향을 받지
않고 사다리를 온전히 유지합니다.

상태는 현재 턴 동안 메모리에 살고(`loop_guard.rs:248` `LoopGuard`) `reset`이
지웁니다(`loop_guard.rs:289` `LoopGuard::reset`). 윈도, 같은 실행 거부 카운트,
순환 사다리를 한꺼번에 버립니다. 두 턴 진입점이 모두 그것을 부르므로, 모델이 지난
턴에 깨뜨린 루프는 이번 턴에 깨끗하게 시작합니다
(`turn_loops.rs:426` `loop_guard.reset`, `turn_streaming_mpsc.rs:67` `loop_guard.reset`). 디스크에 쓰는 것은 없고, 가드는 받은 도구 입력을 절대 변경하지 않습니다.

가드는 `consult_loop_guard`를 통해 두 루프에 연결됩니다
(`turn_loops.rs:79` `consult_loop_guard`). 이 함수는 디스패치 결정 하나의 모든
호출을 기록하고 호출마다 판정 하나를 돌려줍니다. `[features]` 토글도 여기서
확인합니다. `loop_guard = false`이면 아무 호출도 기록하지 않고 모든 판정이
`Allow`입니다(`turn_loops.rs:86` `features.loop_guard`). 비스트리밍 루프는
그룹마다 디스패치 전에 결정 하나를 냅니다
(`turn_loops.rs:71` `consult_loop_guard`). 스트리밍 루프는 구성원 하나짜리
그룹이면 실행 전에, 여러 구성원 그룹이면 어느 구성원이든 시작하기 전에 결정
하나를 냅니다(`turn_streaming_mpsc.rs:1818` `consult_loop_guard`, `turn_streaming_mpsc.rs:1818` `consult_loop_guard`).

거부된 호출은 디스패치 집합에서 빠지지만 자기 인덱스에 터미널 이벤트 하나와 도구
결과 하나를 여전히 남깁니다(`turn_loops.rs:1592` `RoundMember::Blocked`,
`turn_streaming_mpsc.rs:1823` `write_loop_guard_result`). 정지는 남은 결과를 쓰고
안내를 기록한 뒤 턴을 끝냅니다(`turn_loops.rs:396` `record_loop_guard_stop`, `turn_streaming_mpsc.rs:1714` `finish_loop_guard_stop`).

### 가드가 표시하지 않는 것

- 한 파일의 페이지 나눈 읽기. `read`의 대상 정체성에는 요청한 범위 필드가
  들어가므로, 한 파일을 서로 다른 오프셋으로 다섯 번 읽는 것은 서로 다른
  대상입니다(`loop_guard.rs:568` `target_identity`).
- 두 파일을 번갈아 읽기. 구성원이 각각 다른 정체성을 겨냥한 순환은 건너뛰고
  (`loop_guard.rs:492` `every_member_has_a_target`), `read`는 대상 정체성이 있는
  다섯 도구 중 하나입니다(`loop_guard.rs:568` `target_identity`). `ls`,
  `webfetch`, `agentgrep`의 두 대상을 번갈아 도는 것은 해당하지 않아 실제로
  작동합니다. 위의 순환 문단을 보십시오.
- 서로 다른 경로 읽기. 위 경우처럼 서로 다른 대상입니다.
- 한 경로에 대한 두 도구. 실행은 한 도구에 한 입력이므로, 한 파일에서 `read`와
  `lsp_diagnostics`를 번갈아 부르면 경고하지 않습니다 (`loop_guard.rs:455` `identical_run`).
- 스스로 변하는 자원의 반복 폴링. 사다리는 한 번 경고하고 거부도 정지도 하지
  않습니다. 별도 상한이 연속 폴링 24회 뒤에만 턴을 끝냅니다("폴링은 거부 단을
  건너뛰고, 대기에는 상한이 있습니다" 참고).
- 거부 뒤의 다른 서명. 거부하지도 정지하지도 않습니다 (`loop_guard_tests.rs:54` `a_different_signature_after_a_refusal_does_not_stop`).

부정 경우는 테스트로 고정되어 있습니다.
`paginated_reads_of_one_file_are_clean`,
`paginating_one_file_in_repeating_pages_is_clean`,
`alternating_reads_of_two_files_are_clean`,
`reads_of_distinct_paths_are_clean`입니다 (`crates/app-core/src/agent/loop_guard_tests.rs:129` `paginated_reads_of_one_file_are_clean`).

## 토글

두 토글은 모두 `[features]`에 있고, 그 절은 `~/.horon-agent/config.toml`에
있습니다. 기본값은 둘 다 `true`입니다.

| 키 | 기본값 | 출처 |
| --- | --- | --- |
| `parallel_tools` | `true` | `crates/config-types/src/lib.rs:1327` `parallel_tools`, `crates/config-types/src/lib.rs:1367` `parallel_tools` |
| `loop_guard` | `true` | `crates/config-types/src/lib.rs:1330` `loop_guard`, `crates/config-types/src/lib.rs:1368` `loop_guard` |

설정 파일에서 둘 중 하나를 끕니다.

```toml
[features]
parallel_tools = false
loop_guard = false
```

그 절은 설정 파일의 `[features]`입니다
(`crates/base/src/config.rs:524` `features`). 두 키 모두 환경변수
재정의가 없고, 상관없는 `[features]` 키도 두 기본값을 흔들지 않으며, 파싱
테스트가 그것을 고정합니다 (`crates/base/src/config_tests.rs:237` `parallel_tools_and_loop_guard_feature_defaults_and_parse`).

`parallel_tools = false`이면 모든 그룹의 구성원이 정확히 하나라서, 라운드는
동시성이 생기기 전과 같은 방식으로 돕니다
(`turn_loops.rs:53` `plan_tool_round_groups`). 두 루프 모두 불러온 설정에서
플래그를 읽습니다(`turn_loops.rs:1489` `features.parallel_tools`,
`turn_streaming_mpsc.rs:1667` `features.parallel_tools`). `loop_guard`는 두 루프가
모두 부르는 `consult_loop_guard`에서 읽습니다 (`turn_loops.rs:86` `features.loop_guard`).

## 한계

### 그룹이 도는 동안 실행 중인 호출을 백그라운드로 옮기기

백그라운드 도구 요청은(시그널 자체 주석의 이름이 Alt+B/Ctrl+B입니다,
`crates/app-core/src/server/state.rs:15` `background-tool`) 대상
정체성을 나르지 않고, 실행 중 도구 레지스트리는 세션마다 항목 하나만 들므로
기다리는 호출 하나만 지목할 수 있습니다. 여러 구성원 그룹은 항목을 등록하지
않으므로(`turn_streaming_mpsc.rs:1655` `running_tool_registry`), 그룹이 도는 동안
올린 요청은 호출을 옮기는 대신 `moved: false`로 보고합니다
(`turn_streaming_mpsc.rs:1658` `run_group`). 응답은 레지스트리 항목과 시그널로
계산하고(`crates/app-core/src/server/client_lifecycle.rs:1299` `move_tool_to_background`),
이벤트 모양은 `moved`를 담고 `tool_call_id`와 `tool_name`은 없습니다 (`crates/protocol/src/wire/events.rs:298` `BackgroundToolResult`).

시그널은 그룹 뒤에 지우므로, 그룹 중에 들어온 시그널이 다음 구성원 하나짜리 호출을
옮기지 못합니다(`turn_streaming_mpsc.rs:1872` `background_tool_signal.reset`).

구성원 하나짜리 그룹은 인라인 경로를 타고, 그 경로는 기다리는 호출을 등록하므로
여전히 백그라운드로 옮길 수 있습니다
(`turn_streaming_mpsc.rs:1647` `running_tool_registry::register`). 이는 모든
`Ordered` 호출과, `parallel_tools`가 꺼져 있을 때의 모든 호출을 덮습니다. 단 그
호출이 도구에 닿아야 합니다. 잘못된 호출과 SDK가 답한 호출은 등록 없이
해결됩니다.

### 라운드 중 인터럽트

두 루프는 정지에 같은 방식으로 반응하지 않고, 그 차이는 의도적입니다.

`run_turn`에는 라운드 중간 인터럽트가 없습니다. 그곳의 유일한 소프트 인터럽트
지점은 라운드 전체가 끝난 뒤에 돌고
(`turn_loops.rs:1486` `inject_soft_interrupts`), 스케줄러는 취소 시그널 없이
호출됩니다. `turn_loops.rs:1487` `run_group`에 `None`을 넘기고, 루프는 바로 위에서
`parallel_tools`를 읽습니다(`turn_loops.rs:1489` `features.parallel_tools`).
그룹이 도는 동안 도착한 정지는 그룹이 끝난 뒤에 반영되고, 도중에는 반영되지
않습니다.

스트리밍 루프는 라운드 안에서 두 군데 반응합니다. 첫 그룹 뒤의 각 그룹 전에 큐를
직접 읽습니다(`turn_streaming_mpsc.rs:1680` `has_urgent_interrupt`). 여러 구성원
그룹이 도는 동안에는 종료 시그널과 인터럽트 큐를 모두 지켜보는 취소를
`run_group`에 넘깁니다(`turn_streaming_mpsc.rs:2440` `watching_interrupt`,
`tool_round.rs:74` `GroupCancel::new`). 그 감시는 큐에 대한 술어이고, 구성원이
실행 중인 동안 그룹이 `INTERRUPT_POLL_INTERVAL`마다 다시 읽으므로
(`tool_round.rs:58` `INTERRUPT_POLL_INTERVAL`, `tool_round.rs:393` `poll_for_interrupt`), 정지가 나면 끝나지 않은 구성원을 중단시키고 모든 유료 요청을 끝까지 돌리지
않습니다. 25 ms는 폴링 간격이지 보장된 지연이 아닙니다. 정지는 다음 틱에
인지되고, 그 창 안에 끝난 구성원은 결과를 그대로 보고합니다.

폴링이 있는 이유는 큐가 아무것도 깨울 수 없기 때문입니다. 소프트 인터럽트 큐는
`Arc<Mutex<Vec<SoftInterruptMessage>>>`이며
(`crates/agent-runtime/src/lib.rs:51` `SoftInterruptQueue`), 벡터를
감싼 평범한 뮤텍스라 기다릴 것이 없습니다. 알림을 쓰려면 그 별칭을 필드 하나 더
있는 구조체로 바꿔야 하고, 그 변경은 `horon-agent-agent-runtime`에서 큐의 모든
생산자와 소비자가 공유하는 타입을 바꾸는 일입니다. 실행 중인 도구 그룹만 필요한
대기를 위해서입니다. 폴링은 그 공유 타입을 건드리지 않습니다.

두 진입점은 같은 방식으로 끝납니다. 그룹 경계 검사와 그룹 중간 인터럽트가 모두
`finish_streamed_round_interrupt`로 끝나므로
(`turn_streaming_mpsc.rs:1682` `finish_streamed_round_interrupt`), 어느 쪽이 정지를
알아챘든 트랜스크립트와 클라이언트 이벤트, 저장된 세션이 같습니다.

### 훅

`Registry::execute` 안에서 기다리는 훅이 셋이고, 프로세스 전역 비동기 뮤텍스
하나로 직렬화합니다. 입력 변환기는 무엇이 읽기 전에 도구 입력을 바꿀 수 있고
(`tool/mod.rs:1364` `transform_tool_input`), `pre_tool` 게이트는 호출을 막을 수
있으며(`tool/mod.rs:1375` `run_pre_tool_gate`), `post_tool_feedback`은 결과에
텍스트를 다시 넣습니다(`tool/mod.rs:1193` `apply_post_tool_feedback`). 셋 중
겹치기에 가장 나쁜 것은 변환기입니다. 같은 입력을 두 번 고치면 경쟁이 생기고
살아남는 입력이 스케줄링에 달리기 때문입니다. 잠금은 `tool/mod.rs:203` `HOOK_SERIAL`에 설명되어 있습니다.

중요한 결과가 셋입니다.

- 잠금은 그룹 구성원만이 아니라 모든 호출에서 잡습니다. `Registry::execute`는
  호출자가 자기를 묶었는지 알 수 없기 때문입니다.
- 잠금은 훅 호출 동안만 유지되고 도구 자체 실행 동안에는 절대 유지되지 않으므로,
  무관한 도구가 뒤에서 줄 서지 않습니다.
- 잠금은 프로세스 전역입니다. 한 데몬의 두 세션은 훅 호출 하나가 도는 동안 줄을
  섭니다.

떨어져 나간 관찰자는 잠그지 않고 동시에 돌 수 있습니다. `post_tool`
(`tool/mod.rs:1120` `post_tool_event`)과 턴 및 세션 관찰자는
`hooks::dispatch_observer`를 거치고, 이 함수는 분리된 자식을 띄우고 곧바로
돌아옵니다(`crates/base/src/hooks.rs:324` `dispatch_observer`). 그
스크립트 본문은 호출이 돌아온 뒤에 돌기 때문에, 이 프로세스의 어떤 잠금도
그것들의 순서를 정할 수 없습니다.

## 동작을 시험하는 곳

| 영역 | 테스트 파일과 고정하는 테스트 |
| --- | --- |
| `Ordered` 호출 주변의 묶기 | `crates/app-core/src/agent/tool_round_tests.rs` (`grouping_splits_concurrent_runs_around_ordered_calls`, `an_unclassified_call_is_treated_as_ordered`, `write_effect_closes_the_open_group`) |
| 10과 4 상한 | `tool_round_tests.rs` (`grouping_caps_a_concurrent_run_at_ten_members`, `grouping_caps_network_calls_at_four_members`, `a_mixed_group_never_carries_more_than_four_network_members`, `network_effect_limits_a_group_without_a_name_list`, `read_and_network_share_a_group_until_the_network_cap`, `the_network_cap_counts_aliased_network_names`) |
| 효과 해석과 알 수 없는 이름 | `tool_round_tests.rs` (`unknown_tool_effect_is_a_barrier`, `plan_round_reads_the_effects_of_each_call_from_its_input`) |
| 순서가 있는 결과, 패닉 격리, 취소 | `tool_round_tests.rs` |
| 라운드 소유 예산 | `crates/app-core/src/agent/round_budget_tests.rs` (`the_counter_survives_a_group_boundary`, `first_output_is_admitted_and_the_second_that_crosses_is_refused`) |
| 사다리와 그 경계 | `crates/app-core/src/agent/loop_guard_tests.rs` (`identical_run_warns_refuses_then_stops`, `a_different_signature_after_a_refusal_does_not_stop`) |
| 실행은 한 도구에 한 입력 | `loop_guard_tests.rs` (`alternating_tools_on_one_path_never_warn`, `one_tools_refusal_does_not_stop_another_tool`, `polls_before_a_repeated_read_do_not_soften_it`, `reset_clears_the_poll_ladder`) |
| 폴링은 계속 돌고 변경 호출은 그렇지 않음 | `loop_guard_tests.rs` (`identical_bg_polls_warn_once_and_never_refuse_or_stop`, `every_polling_bg_action_keeps_running`, `identical_output_readers_keep_running`, `identical_status_style_mcp_calls_keep_running`, `a_native_mcp_status_call_keeps_running`, `a_mutating_bg_action_keeps_the_full_ladder`, `a_non_status_mcp_tool_keeps_the_full_ladder`, `a_cycle_of_polls_warns_once_and_keeps_running`, `a_cycle_with_a_non_poll_member_still_escalates`) |
| 대기를 제한하는 폴링 상한 | `loop_guard_tests.rs` (`consecutive_polls_are_capped_and_the_cap_ends_the_turn`, `the_polling_cap_counts_polls_whose_input_changes`) |
| 순환 사다리, 그 해제, 턴별 초기화 | `loop_guard_tests.rs` (`repeating_cycle_warns_then_refuses_then_stops`, `changing_one_step_of_a_refused_cycle_is_allowed`, `cycle_ladder_is_cleared_by_reset`) |
| 기다리는 훅의 직렬화, 변환기 포함 | `crates/app-core/src/tool/hook_serialization_tests.rs` (`concurrent_reads_serialize_post_tool_feedback_hooks`, `concurrent_reads_serialize_the_input_transform_hook`, `post_tool_observer_stays_detached`) |
| `intent` 정규화, 페이지 나누기와 A/B 부정 경우, 윈도 축출 | `loop_guard_tests.rs` (`intent_does_not_change_the_signature`, `paginated_reads_of_one_file_are_clean`, `paginating_one_file_in_repeating_pages_is_clean`, `alternating_reads_of_two_files_are_clean`, `reads_of_distinct_paths_are_clean`, `window_evicts_and_caps_the_reported_count`) |
| 실제 에이전트 상태를 거치는 비스트리밍 라운드 | `crates/app-core/src/agent/turn_loops_round_tests.rs` |
| 두 토글, 기본값과 파싱 | `crates/base/src/config_tests.rs:237` `parallel_tools_and_loop_guard_feature_defaults_and_parse` |
| 도구 실행에 쓰는 공유 레지스트리 클론 | `crates/app-core/src/tool/compaction_sharing_tests.rs`, `crates/app-core/src/agent/streaming_clone_compaction_tests.rs` |
| 클라이언트가 받는 Alt+B 응답 | `crates/app-core/src/agent/turn_streaming_mpsc_interrupt_tests.rs` (`a_background_request_during_a_group_is_refused_visibly`, `a_lone_ordered_bash_is_still_detachable`) |

## 인용을 쓰는 방식

이 문서의 모든 인용은 대상 기호를 그 기호가 있는 줄 옆에 적습니다. 파일, 줄,
그리고 자기 코드 스팬에 담긴 기호 순서입니다. 독자는 기호를 grep합니다.
이 문서를 작성할 때 사용한 검사 스크립트는 저장소에 포함되지 않습니다(`.omo/`는 추적하지 않음). 그
스크립트는 인용한 파일 안에서 기호를 찾아 인용한 줄에서 15줄 넘게 떨어져 있으면 실패했습니다.
지금은 CI가 이 검사를 돌리지 않으므로, 줄 번호가 어긋났으면 기호를 grep해 위치를 다시 찾습니다.
기호가 아니라 모듈 문서 블록을 가리키는
인용 셋은 줄 앞에 `~`를 붙여 근사임을 표시합니다.
