# AI Council

사람 + 여러 AI(GPT, Claude, Gemini, Grok 중 2~4)가 한 방에서 대화하는 실험 프로젝트다. 핵심 spike는 다음 하나를 검증한다.

> 모든 AI가 최신 Room Event를 처리한 뒤 스스로 `PASS` 또는 `REQUEST_FLOOR`를 정하고, 내용과 무관한 발언권 중재만으로 자연스러운 단체대화가 만들어지는가?

실험 계약은 [EXPERIMENT.md](EXPERIMENT.md)에 먼저 고정했다. 현재 구현은 in-memory only이며 UI, DB, Judge, router, 장기 memory, voice는 없다.

## 비용 없이 먼저 실행

기본값은 네트워크나 유료 API를 전혀 사용하지 않는 결정론적 mock이다.

```bash
cargo run --manifest-path rust/Cargo.toml -p council-core -- \
  --provider mock \
  --once "AI 여러 명이 실제 대화하면 어떤 가치가 있을까?"
```

대화형 실행:

```bash
cargo run --manifest-path rust/Cargo.toml -p council-core
```

명령은 `/metrics`, `/state`, `/rate 1..5 선택적 메모`, `/quit`이다. PASS는 공개 `RoomEvent`가 아니므로 대화 본문에는 나타나지 않고 `[control]` trace에서만 확인된다.

세션이 하나 이상의 발언을 만들면 종료 시 transcript(이벤트 전문 + control trace + 지표)가 `outputs/session-<unix>.md`로 저장된다. EXPERIMENT.md §6의 10개 주제 사람 검토가 이 파일을 근거로 한다. `--transcript PATH`로 경로를 바꾸고 `--no-transcript`로 끈다.

## 구독으로 실제 모델 실행

로컬 CLI가 각자의 구독으로 로그인되어 있다면 별도의 API 키 없이 실행할 수 있다: Codex(`codex`, ChatGPT 구독), Claude Code(`claude`, claude.ai 구독), Grok(`grok`, grok.com 구독), Antigravity(`agy`, Google 계정).

```bash
cargo run --manifest-path rust/Cargo.toml -p council-core -- \
  --provider subscription
```

참가 AI는 `--agents`로 고른다 (기본 `gpt,claude`, 순서는 항상 전역 고정 순서 GPT→Claude→Gemini→Grok을 따른다).

```bash
cargo run --manifest-path rust/Cargo.toml -p council-core -- \
  --provider subscription --agents gpt,claude,gemini,grok
```

모델을 호출하거나 구독 한도를 쓰지 않고 로그인·실행 파일만 점검:

```bash
cargo run --manifest-path rust/Cargo.toml -p council-core -- \
  --provider subscription \
  --check-providers
```

이 모드는 시작할 때 두 CLI의 로그인 방식을 검사하고, child process에서 `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`와 외부 cloud provider 변수를 제거한다. provider session은 저장하지 않으며 매 판단은 ephemeral/no-session-persistence 프로세스에서 전체 Room snapshot으로 다시 시작한다.

선택 모델·effort (CLI가 지원하는 항목만):

```bash
export CODEX_SUBSCRIPTION_MODEL="..."                  # 생략하면 Codex 구독 기본값
export CODEX_SUBSCRIPTION_EFFORT="high"                # model_reasoning_effort로 전달
export CLAUDE_SUBSCRIPTION_MODEL="sonnet"              # 기본값 (effort 개념 없음)
export GEMINI_SUBSCRIPTION_MODEL="gemini-3.7-flash-high"  # 기본값
export GEMINI_SUBSCRIPTION_EFFORT="high"               # agy --effort
export GROK_SUBSCRIPTION_MODEL="..."                   # 생략하면 Grok CLI 기본값
export GROK_SUBSCRIPTION_EFFORT="high"                 # grok --reasoning-effort
```

이 방식은 **별도 API 종량제 호출을 피하지만 무제한은 아니다.** Codex는 ChatGPT 플랜 한도를, `claude -p`는 현재 구독자에게 제공되는 별도 월간 Agent SDK credit 또는 적용되는 구독 한도를 소비한다. Claude 측 credit은 계정에서 별도 opt-in이 필요할 수 있다. 한도/credit이 끝나면 멈추도록 계정의 추가 usage credits 또는 자동 충전을 비활성화해야 비용 상한이 확실하다. 또한 CLI를 매 inference마다 실행하므로 API보다 느리고, 배포용 backend가 아니라 로컬 spike 전용이다.

## 실제 GPT + Claude 실행 — 유료

API 키를 만드는 행위 자체보다 **키로 모델을 호출한 사용량에 비용이 발생**한다. ChatGPT 구독과 OpenAI API 결제, Claude 구독과 Claude API 결제는 각각 별도다. 이 spike는 `--provider live`를 명시하지 않으면 실제 API를 호출하지 않는다.

```bash
export OPENAI_API_KEY="..."
export ANTHROPIC_API_KEY="..."
cargo run --manifest-path rust/Cargo.toml -p council-core -- --provider live
```

선택적으로 모델을 바꿀 수 있다.

```bash
export OPENAI_MODEL="gpt-5.4-mini"
export ANTHROPIC_MODEL="claude-sonnet-4-6"
```

기본 모델의 2026-08-21 공개 단가는 입력/출력 100만 토큰당 GPT-5.4 mini가 `$0.75 / $4.50`, Claude Sonnet 4.6이 `$3 / $15`다. 가격은 변할 수 있으므로 실행 전 [OpenAI 가격표](https://platform.openai.com/pricing)와 [Claude 모델 가격 안내](https://www.anthropic.com/news/claude-sonnet-4-6)를 다시 확인한다.

한 사용자 발언은 최소 두 번의 모델 판단을 부른다. AI가 실험 상한인 3회 연속 발언하면 `2 evaluations + 3 speeches + 3 re-evaluations = 최대 8 API calls`다. 전체 event log를 매번 다시 보내므로 대화가 길수록 입력 토큰도 증가한다.

## 동작

```text
commit RoomEvent
  → GPT/Claude가 immutable Room snapshot 처리
  → Listening Barrier (둘 다 last_heard_event 갱신)
  → PASS / REQUEST_FLOOR
  → requester가 있으면 round-robin floor
  → 한 AI만 speak
  → 새 RoomEvent commit + 이전 intent 전부 폐기
  → 새 event를 다시 처리
```

- `Room.event_log`와 `AgentState`만 source of truth다.
- OpenAI Responses API의 response id나 Claude provider session을 보관하지 않는다.
- 사람 event는 GPT와 Claude를 병렬 호출한다.
- AI event의 저자는 sync-only, 다른 AI는 새 inference를 한다.
- barrier 중 한 provider라도 실패하면 fail closed하고 아무에게도 발언권을 주지 않는다.
- 동시 요청은 고정 순서의 round-robin만 사용하며 내용, confidence, latency를 보지 않는다.
- AI 발언 3회 뒤에도 requester가 있으면 마지막 event까지 모두 처리한 후 `AI_STREAK_LIMIT`으로 멈춘다.

## 코드 경계

- `engine.rs`: Room commit, barrier, intent invalidation, floor, streak limit
- `model.rs`: `RoomEvent`, `AgentState`, `Intent`
- `providers/openai.rs`: OpenAI Responses API adapter
- `providers/anthropic.rs`: Claude Messages API adapter
- `providers/mock.rs`: 무료 결정론적 protocol demo
- `providers/subscription.rs`: ChatGPT/Claude/Grok/Antigravity 구독 로그인 CLI adapter
- `metrics.rs`: PASS율, 동시 REQUEST율, AI streak, 사람 평점
- `tests/protocol.rs`: 핵심 protocol invariants

## 검증

저장소 루트에서:

```bash
scripts/fmt.sh --check
cd rust
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

테스트는 두 adapter의 병렬 barrier 도달, losing intent 폐기와 재판단, round-robin, provider 실패 시 fail-closed, 마지막 AI event 처리, 지표 계산, transcript 렌더링을 검증한다. 이 테스트는 orchestration의 증거이지 자연스러운 실제 대화의 증거가 아니다. 실제 품질 판단에는 서로 다른 주제의 live 세션과 사람의 `/rate` 평가가 필요하다.

## 웹 UI

실제 활용을 위한 로컬 웹 UI가 있다 (실험 계약 밖의 활용 레이어이며 프로토콜은 CLI와 동일한 엔진을 쓴다).

```bash
cargo run --manifest-path rust/Cargo.toml -p council-core --bin council-web
```

기본값은 `--provider subscription --agents claude,gemini,grok --port 8787`이고, http://127.0.0.1:8787 에서 채팅 버블·발언권 trace·지표·자연스러움 평점(1~5 버튼)을 제공한다. AI 발언은 커밋되는 즉시 화면에 나타나고, transcript는 `outputs/web-session-<unix>.md`로 자동 저장된다.

대화 화면에서:

- **심의 진행 상황** — 각 AI의 판단(생각 중 → PASS/REQUEST → 발언 작성 중)이 실시간 칩으로 표시된다.
- **REQUEST_FLOOR 이유** — 발언권을 요청한 AI의 내부 이유가 control trace 아래 `↳`로 표시되고 transcript에도 저장된다.
- **지목 발언** — "전체에게" 대신 특정 AI를 골라 물으면 그 사이클의 첫 발언권이 그 AI에게 간다 (EXPERIMENT.md §4의 사람 우선 규칙 확장).
- **중단** — 심의 중 중단 버튼으로 취소한다. 이미 커밋된 발언은 유지된다.
- **기록** — 지난 세션(JSON sidecar)을 열람하고, "이 대화 이어가기"로 그 방을 현재 참가자 구성으로 복원한다.

설정 패널(우상단)에서:

- **AI별 구독 인증 상태**를 검사해 표시한다 (모델 호출 없음). 로그인 자체는 각 CLI에서 한다.
- **참가자·모델·effort**를 좌석별로 고르고 AI 연속발언 상한을 정한 뒤 **새 세션 시작**으로 적용한다 (이전 대화는 transcript로 남고 방은 초기화된다).
- **세션 예산** — 비용($)·토큰 상한을 정하면 도달 시 새 발언 접수를 막는다 (0 = 무제한).
- **이번 세션 사용량**(호출 수·입출력 토큰·CLI가 보고한 비용)을 AI별로 누적 표시한다. 잔여 구독 한도는 CLI들이 헤드리스로 노출하지 않으므로 표시하지 않는다 — 한도 초과는 fail-closed 오류로 드러나며 오류 문구에 재설정 시각이 담겨 온다.

## 검증 현황 (2026-08-21)

| 항목 | 상태 |
| --- | --- |
| fmt / clippy(-D warnings) / 테스트 9개 | 통과 |
| mock 전체 루프 + transcript 저장 | 통과 |
| 구독 인증 사전점검 (`--check-providers`) | 통과 |
| GPT 구독 adapter 실제 판단 | 2026-08-21 Codex 세션에서 통과 |
| Claude 구독 adapter evaluate/speak | 2026-08-21 실제 CLI 호출로 통과 (`structured_output` 판단 + 자연어 발언 확인) |
| barrier fail-closed (실제 provider 오류) | 통과 — GPT 한도 초과 시 발언권 미부여 확인 |
| GPT+Claude 실제 3자 세션 | **대기** — ChatGPT 구독 사용량 한도, 2026-08-27 17:08 재설정 후 실행 |
| 대화 품질 acceptance (주제 10개 + `/rate`) | **대기** — 위 세션 재개 후 진행 |
