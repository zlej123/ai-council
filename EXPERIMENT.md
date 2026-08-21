# Council Core Spike — 1페이지 실험 사양

## 1. 질문과 성공 조건

**질문:** You + GPT + Claude가 같은 발언 기록을 실제로 처리하고, 각 AI가 스스로 침묵하거나 발언권을 요청할 때, 단순한 다중 답변이 아니라 자연스러운 3자 대화가 만들어지는가?

성공은 기능 완성이 아니라 아래 관찰이 가능한 상태다.

- 모든 `RoomEvent` 뒤에 두 `AgentState.last_heard_event`가 최신 event id에 도달한다.
- 사람의 발언에는 GPT와 Claude를 **동시에 실제 호출**하여 각각 `PASS` 또는 `REQUEST_FLOOR`를 받는다.
- AI 발언에는 다른 AI를 실제 호출하고, 발언한 AI는 자기 event를 동기화만 한다.
- 둘 다 요청해도 내용·점수·응답 속도를 보지 않는 round-robin으로 한 명만 말한다.
- 새 발언이 commit되는 순간 이전 event에 근거한 모든 intent는 폐기된다.
- PASS율, 동시 REQUEST율, AI 연속발언 길이와 사람의 자연스러움 평점을 한 세션에서 볼 수 있다.

## 2. 범위

포함: 단일 in-memory room, You/GPT/Claude, 텍스트 CLI, 병렬 listening barrier, provider별 adapter, 기계적 floor arbitration, 세션 지표, 결정론적 mock 실험, 구독 로그인 CLI 실험.

제외: UI, DB/복구, Judge/router, 장기·요약 memory, 음성, 도구 사용, 스트리밍, 인증, 다중 room, 비용 최적화, production retry/failover.

## 3. Source of truth와 상태

- `CouncilRoom.event_log`: commit된 공개 대화의 유일한 원본. PASS와 intent는 여기에 쓰지 않는다.
- `AgentState`: `last_heard_event`, 현재 intent와 그 근거 event id, 판단 횟수/오류.
- provider 응답 ID·conversation/session은 저장하지 않는다. 매 inference 입력은 그 시점의 **전체 Room snapshot**으로 다시 만든다.
- `Intent(basis_event_id, PASS | REQUEST_FLOOR)`는 해당 event에만 유효하다. 더 최신 event가 생기면 사용할 수 없다.

## 4. 한 사이클의 프로토콜

1. 발언을 다음 연속 id의 `RoomEvent`로 commit하고 기존 intent를 전부 지운다.
2. immutable room snapshot을 두 adapter에 broadcast한다.
3. 각 AI가 event를 처리한다. 저자가 아닌 AI는 provider inference로 `PASS/REQUEST_FLOOR`를 결정한다. AI 저자는 자기 event를 sync-only 처리한다.
4. 두 `last_heard_event`가 모두 최신 id가 되어야 listening barrier가 완료된다. 하나라도 실패하면 **fail closed**: 발언권을 주지 않고 사용자에게 오류를 반환한다.
5. 유효한 requester가 없으면 `QUIESCENT`로 사람을 기다린다.
6. requester가 있으면 고정 순서 `[GPT, Claude]`의 round-robin cursor만으로 한 명을 고른다. 의미, confidence, latency, provider는 사용하지 않는다.
7. 선택된 adapter가 현재 room snapshot과 자기 intent를 받아 짧은 발언을 생성한다. commit 후 1번으로 돌아간다.
8. 사용자 발언 하나 뒤 AI 발언이 3개에 도달하면 `AI_STREAK_LIMIT`로 멈춘다. 이는 제품 정책이 아니라 무한 루프를 막는 spike 안전장치다.

## 5. 모델 계약

공통 규칙은 “유용한 새 정보·의미 있는 반대·중요한 수정·진행에 필요한 질문”이 있을 때만 요청하고, 반복·단순 동의는 PASS하며, 기본 1–4문장으로 답하는 것이다. 판단 응답은 엄격한 JSON 객체 하나다.

```json
{"decision":"PASS"}
```

또는

```json
{"decision":"REQUEST_FLOOR","reason":"짧은 내부 관측 이유"}
```

종량제 모드에서 OpenAI adapter는 Responses API, Claude adapter는 stateless Messages API를 각각 직접 감싼다. 기본 모델은 `gpt-5.4-mini`, `claude-sonnet-4-6`이며 환경변수로 교체할 수 있다. 키는 `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`에서만 읽는다.

구독 모드는 별도 adapter로 `codex exec --ephemeral`과 `claude --print --no-session-persistence`를 호출한다. 시작 전에 Codex가 ChatGPT로, Claude가 claude.ai 구독으로 로그인했는지 검사하며 child process에서 API key·외부 cloud 인증 환경변수를 제거한다. 이 모드는 별도 API 종량제 대신 구독 한도 또는 구독자용 월간 credit을 소비한다. 추가 usage credits가 계정에서 활성화된 경우 생길 수 있는 초과 결제까지 로컬 코드가 차단하지는 못한다.

## 6. 지표와 판정

- **PASS율** = PASS 판단 수 / 전체 모델 판단 수.
- **동시 REQUEST율** = 두 AI가 모두 평가한 barrier 중 둘 다 REQUEST한 수 / 해당 barrier 수.
- **AI 연속발언 길이** = 사람 발언 사이의 AI event 개수. 평균·최대·분포를 출력한다.
- **자연스러운 단체대화 체감** = 사용자가 `/rate 1..5 선택적 메모`로 기록한 평균과 원문. 자동 품질 점수로 대체하지 않는다.

프로토콜 acceptance는 단위/통합 테스트로 barrier, stale-intent 폐기, round-robin, fail-closed, 지표를 증명한다. 대화 품질 acceptance는 서로 다른 실제 주제 최소 10개를 사람이 읽고, 자연스러움 평균·PASS율·동시 REQUEST율·streak 분포를 함께 검토한 뒤 결정한다. 테스트 통과는 자연스러운 대화의 증거가 아니다.

사람 검토를 위해 각 세션은 종료 시 `outputs/session-<unix>.md`로 export된다(`--transcript PATH`로 경로 지정, `--no-transcript`로 비활성). 이 파일은 검토용 사본일 뿐이며 in-memory `Room.event_log`가 유일한 source of truth라는 원칙은 유지된다.
