# CLAUDE.md

Council Core Spike — You + GPT + Claude 3자 대화에서 발언권 중재만으로 자연스러운
단체대화가 되는지 검증하는 Rust 실험. 계약은 [EXPERIMENT.md](EXPERIMENT.md)에 고정되어
있고, 범위를 바꾸는 변경은 그 문서를 먼저 수정한 뒤에만 한다.

## 구조

- Rust workspace는 `rust/` 아래에 있다 (`rust/council-core`).
- `src/`(레포 루트)는 Rust 코드가 아니라 공유 계약 파일이다: `intent.schema.json`,
  `council_rules.txt` — 두 파일 모두 `include_str!`로 바이너리에 들어간다.
- `outputs/`는 세션 transcript 저장 위치. 품질 검토에 쓴 세션만 커밋한다.

## 검증 (레포 루트에서)

```bash
scripts/fmt.sh --check
cd rust && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## 실행 모드

- `--provider mock` (기본): 무료, 네트워크 없음. 프로토콜 확인용.
- `--provider subscription`: 로컬 `codex` + `claude` CLI를 구독 로그인으로 호출.
  **구독 한도를 소비한다** — 한도 초과 시 barrier가 fail-closed로 멈추는 것이 정상이다.
- `--provider live`: `OPENAI_API_KEY`/`ANTHROPIC_API_KEY` 종량제. 명시하지 않으면 절대
  호출되지 않는다.

## 주의

- Claude Code 세션 안에서 subscription 모드를 실행하면 child `claude` 프로세스가
  같은 구독 한도를 공유한다. 긴 세션 전에 한도 여유를 확인할 것.
- 프로토콜 테스트 통과는 대화 품질의 증거가 아니다 (EXPERIMENT.md §6). 품질 판정은
  live 주제 10개 + `/rate` 기록으로만 한다.
