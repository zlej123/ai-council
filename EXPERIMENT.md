# AI Council — Core Spike 1페이지 실험 사양

## 1. 질문과 성공 조건

**질문:** You + 여러 AI(GPT, Claude, Gemini, Grok 중 2~4)가 같은 발언 기록을 실제로 처리하고, 각 AI가 스스로 침묵하거나 발언권을 요청할 때, 단순한 다중 답변이 아니라 자연스러운 단체대화가 만들어지는가?

참가 AI는 `--agents` 플래그로 고르며(기본 `gpt,claude`), 전역 고정 순서는 `[GPT, Claude, Gemini, Grok]`이다. 프로토콜의 다른 어떤 규칙도 참가 수에 의존하지 않는다.

성공은 기능 완성이 아니라 아래 관찰이 가능한 상태다.

- 모든 `RoomEvent` 뒤에 참가 AI 전원의 `AgentState.last_heard_event`가 최신 event id에 도달한다.
- 사람의 발언에는 참가 AI 전원을 **동시에 실제 호출**하여 각각 `PASS` 또는 `REQUEST_FLOOR`를 받는다.
- AI 발언에는 나머지 AI를 실제 호출하고, 발언한 AI는 자기 event를 동기화만 한다.
- 여럿이 요청해도 내용·점수·응답 속도를 보지 않는 round-robin으로 한 명만 말한다.
- 새 발언이 commit되는 순간 이전 event에 근거한 모든 intent는 폐기된다.
- PASS율, 동시 REQUEST율, AI 연속발언 길이와 사람의 자연스러움 평점을 한 세션에서 볼 수 있다.

## 2. 범위

포함: 단일 in-memory room, You + 선택된 AI 로스터(GPT/Claude/Gemini/Grok), 텍스트 CLI, 병렬 listening barrier, provider별 adapter, 기계적 floor arbitration, 세션 지표, 결정론적 mock 실험, 구독 로그인 CLI 실험, 사이클 취소.

제외(실험 계약): UI, DB/복구, Judge/router, 장기·요약 memory, 음성, 도구 사용, 스트리밍, 인증, 다중 room, 비용 최적화, production retry/failover. 단, 활용 레이어(README의 웹 UI)는 검토용 transcript 사본(JSON sidecar)으로 지난 방을 **다시 시드**(`Council::seed_room`)할 수 있다 — 이는 저장소를 source of truth로 삼는 복구가 아니라 새 in-memory room을 과거 event로 시작하는 것이며, 시드된 event는 listening barrier를 거치지 않고 다음 사람 발언에서 전원이 재평가한다.

## 3. Source of truth와 상태

- `CouncilRoom.event_log`: commit된 공개 대화의 유일한 원본. PASS와 intent는 여기에 쓰지 않는다.
- `AgentState`: `last_heard_event`, 현재 intent와 그 근거 event id, 판단 횟수/오류.
- provider 응답 ID·conversation/session은 저장하지 않는다. 매 inference 입력은 그 시점의 **전체 Room snapshot**으로 다시 만든다.
- `Intent(basis_event_id, PASS | REQUEST_FLOOR)`는 해당 event에만 유효하다. 더 최신 event가 생기면 사용할 수 없다.

## 4. 한 사이클의 프로토콜

1. 발언을 다음 연속 id의 `RoomEvent`로 commit하고 기존 intent를 전부 지운다.
2. immutable room snapshot을 참가 adapter 전원에 broadcast한다.
3. 각 AI가 event를 처리한다. 저자가 아닌 AI는 provider inference로 `PASS/REQUEST_FLOOR`를 결정한다. AI 저자는 자기 event를 sync-only 처리한다.
4. 참가 AI 전원의 `last_heard_event`가 최신 id가 되어야 listening barrier가 완료된다. 하나라도 실패하면 **fail closed**: 발언권을 주지 않고 사용자에게 오류를 반환한다.
5. 유효한 requester가 없으면 `QUIESCENT`로 사람을 기다린다.
6. requester가 있으면 고정 순서 `[GPT, Claude, Gemini, Grok]`(참가 로스터로 filter)의 round-robin cursor만으로 한 명을 고른다. 의미, confidence, latency, provider는 사용하지 않는다. 예외는 하나다: **사람이 특정 AI를 지목한 발언**은 "사람이 우선한다" 규칙의 확장으로, 그 사이클의 첫 발언권을 지목된 AI에게 준다(cursor는 움직이지 않으며, 이후 발언권은 다시 일반 중재를 따른다). 이것은 사람의 명시적 지시이지 시스템의 내용 판단이 아니다.
7. 선택된 adapter가 현재 room snapshot과 자기 intent를 받아 짧은 발언을 생성한다. commit 후 1번으로 돌아간다.
8. 사용자 발언 하나 뒤 AI 발언이 3개에 도달하면 `AI_STREAK_LIMIT`로 멈춘다. 이는 제품 정책이 아니라 무한 루프를 막는 spike 안전장치다. 사용자가 사이클을 중단하면 `CANCELLED`로 멈추며, 이미 commit된 event는 유지되고 round-robin cursor는 실제로 commit된 발언에 대해서만 전진한다.

## 5. 모델 계약

공통 규칙은 “유용한 새 정보·의미 있는 반대·중요한 수정·진행에 필요한 질문”이 있을 때만 요청하고, 반복·단순 동의는 PASS하며, 기본 1–4문장으로 답하는 것이다. 사람이 특정 AI를 지목해 물었고 그 AI가 방금 답했다면, 나머지 AI는 사람이 먼저 반응할 수 있도록 PASS를 우선한다 — 그대로 두면 사람을 오도할 오류의 정정만 예외다. 이것은 §4 지목 발언과 같은 “사람이 우선한다” 규칙의 모델 측 확장이며, 발언권 중재는 여전히 내용을 판단하지 않는다. 판단 응답은 엄격한 JSON 객체 하나다.

```json
{"decision":"PASS"}
```

또는

```json
{"decision":"REQUEST_FLOOR","reason":"짧은 내부 관측 이유"}
```

종량제 모드에서 OpenAI adapter는 Responses API, Claude adapter는 stateless Messages API를 각각 직접 감싼다. 기본 모델은 `gpt-5.4-mini`, `claude-sonnet-4-6`이며 환경변수로 교체할 수 있다. 키는 `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`에서만 읽는다.

구독 모드는 별도 adapter로 `codex exec --ephemeral`, `claude --print --no-session-persistence`, `grok --single`(grok.com 구독), `agy --print`(Antigravity, Google 계정)를 호출한다. 시작 전에 각 CLI의 구독 로그인 상태를 검사하며 child process에서 API key·외부 cloud 인증 환경변수를 제거한다. 이 모드는 별도 API 종량제 대신 구독 한도 또는 구독자용 월간 credit을 소비한다. 추가 usage credits가 계정에서 활성화된 경우 생길 수 있는 초과 결제까지 로컬 코드가 차단하지는 못한다.

## 6. 지표와 판정

- **PASS율** = PASS 판단 수 / 전체 모델 판단 수.
- **동시 REQUEST율** = 평가자가 2명 이상인 barrier 중 2명 이상이 REQUEST한 수 / 해당 barrier 수. (2-AI 로스터에서는 기존 "둘 다 평가, 둘 다 REQUEST" 정의와 동일하다.)
- **AI 연속발언 길이** = 사람 발언 사이의 AI event 개수. 평균·최대·분포를 출력한다.
- **자연스러운 단체대화 체감** = 사용자가 `/rate 1..5 선택적 메모`로 기록한 평균과 원문. 자동 품질 점수로 대체하지 않는다.

프로토콜 acceptance는 단위/통합 테스트로 barrier, stale-intent 폐기, round-robin, fail-closed, 지표를 증명한다. 대화 품질 acceptance는 서로 다른 실제 주제 최소 10개를 사람이 읽고, 자연스러움 평균·PASS율·동시 REQUEST율·streak 분포를 함께 검토한 뒤 결정한다. 테스트 통과는 자연스러운 대화의 증거가 아니다.

사람 검토를 위해 각 세션은 종료 시 `outputs/session-<unix>.md`로 export된다(`--transcript PATH`로 경로 지정, `--no-transcript`로 비활성). 이 파일은 검토용 사본일 뿐이며 in-memory `Room.event_log`가 유일한 source of truth라는 원칙은 유지된다.

## 7. v2 확장: 일하는 카운슬 (2026-08-29)

§1–6은 v1 계약으로 보존된다. v1의 질문("발언권 중재만으로 자연스러운 단체대화가
되는가")과 그 판정 기준은 이 확장으로 바뀌지 않으며, v1 규칙으로 실행된 세션의
기록과 지표도 그대로 남는다.

**v2 질문:** 같은 발언권 중재 위에서 발언 턴이 도구를 갖게 되었을 때, 여러 AI가
사람 앞에서 자연스럽게 **협업**하는가 — 스스로 근거를 찾고, 요청받은 작업을
수행하고, 결과를 대화로 보고하는가.

### 턴 능력

- **판단 턴(PASS/REQUEST_FLOOR)은 v1 그대로 도구가 없다.** listening barrier의
  비용과 지연을 낮게 유지하는 것이 우선이다.
- **발언 턴은 도구를 가진다 — 단, 각 좌석의 CLI가 기계적으로 가둘 수 있는 만큼만.**
  - 웹 검색과 워크스페이스·artifacts 폴더의 읽기/검색은 가둘 수 있는 좌석에 항상
    허용된다.
  - 파일 생성과 코드 실행은 사람의 명시적 요청이 있을 때만이며, 그 좌석의 실행
    환경이 **세션 전용 artifacts 폴더 안으로 기계적으로 격리**할 수 있을 때만
    부여된다. 격리할 수 없는 능력은 규칙으로 금지하는 대신 **부여하지 않는다** —
    도구 없이 규칙만 있으면 모델이 도구를 쓴 척할 유인이 생기기 때문이다.
  - 부여는 좌석별로 다르며, 코드의 `providers::seat_tools`가 유일한 정의다. 발언
    프롬프트와 CLI 인자는 모두 거기서 읽으므로 모델은 자기가 실제로 가진 도구를
    정확히 듣는다(테스트로 고정). 2026-08-29 기준:

    | 좌석 | 웹 검색 | 읽기 | 파일 생성 | 코드 실행 | 격리 방식 |
    | --- | --- | --- | --- | --- | --- |
    | GPT (Codex) | ○ | ○ (전체 디스크 — 읽기는 가둘 수 없어 규칙만) | ○ | ○ | 쓰기·실행을 artifacts로 샌드박스 |
    | Claude | ○ | ○ | 워크스페이스가 **없을 때만** | × | `--restricted` 경계 하나(읽기+쓰기 통합)라 워크스페이스가 있으면 Write를 빼서 읽기전용을 구조적으로 보장 |
    | Gemini (Antigravity) | ○ | × | × | × | 기계적 격리 수단 없음 → 파일 도구 미부여 |
    | Grok | ○ | ○ | ○ | ○ | 커널 샌드박스: 워크스페이스 읽기전용, artifacts만 쓰기 |

  - 판단 턴은 좌석과 무관하게 도구가 없으며, 프롬프트가 그 사실을 명시한다.

### 워크스페이스와 산출물

- **워크스페이스**는 사람이 세션에 지정하는 읽기 전용 폴더다. 어떤 턴도 이 폴더를
  변경할 수 없다.
- **artifacts 폴더**는 세션마다 새로 만들어지는 유일한 쓰기 가능 영역이다.
- 발언은 v1과 같이 기본 1–4문장을 유지하고, 근거는 URL과 파일 경로로 **인용**한다.
  다른 AI도 읽기 도구가 있으므로 인용된 파일을 직접 열어 검증하거나 반박할 수
  있다 — 이것이 v2의 협업 검증 메커니즘이며, `RoomEvent` 구조와 발언권 중재는
  바꾸지 않는다.

### v2 범위 제외

쓰기 승인 팝업, 워크스페이스(사람 폴더) 직접 쓰기, 도구 작업 로그의 실시간
스트리밍, 도구를 쓰는 판단 턴, 세션 간 artifacts 공유.

### 관찰과 판정

v1 지표(§6)에 더해 발언 턴 소요시간 분포, 도구를 사용한 발언의 비율, 그리고
**인용 검증 발언**(다른 AI가 앞선 발언의 인용을 실제로 열어 보고 반응한 발언)을
관찰한다. 협업의 자연스러움 판정은 v1과 같이 자동 점수가 아니라 사람이 한다.
