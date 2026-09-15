# AI Council

<p align="center">
  <img src="docs/council-meeting.png" alt="Illustration: a human and four AIs — GPT, Claude, Gemini and Grok — around a round table mid-discussion, with a marker in the middle pointing at whoever currently holds the floor" width="100%">
</p>

An experiment in putting a human and several AIs (2–4 of GPT, Claude, Gemini, Grok) in one room. The core spike tests exactly one thing:

> Once every AI has processed the latest room event and decided for itself to `PASS` or `REQUEST_FLOOR`, does floor arbitration that ignores content alone produce a natural group conversation?

The experiment contract was fixed up front in [EXPERIMENT.md](EXPERIMENT.md). The current implementation is in-memory only — no UI, no DB, no judge, no router, no long-term memory, no voice.

## Run it for free first

The default is a deterministic mock that touches no network and no paid API.

```bash
cargo run --manifest-path rust/Cargo.toml -p council-core -- \
  --provider mock \
  --once "What is actually valuable about several AIs talking to each other?"
```

Interactive:

```bash
cargo run --manifest-path rust/Cargo.toml -p council-core
```

The commands are `/metrics`, `/state`, `/rate 1..5 optional note`, and `/quit`. A PASS is not a public `RoomEvent`, so it never appears in the conversation body — you only see it in the `[control]` trace.

If a session produces at least one utterance, a transcript (full events + control trace + metrics) is written to `outputs/session-<unix>.md` on exit. The 10-topic human review in EXPERIMENT.md §6 is based on these files. Use `--transcript PATH` to change the location and `--no-transcript` to turn it off.

## Run real models on your subscriptions

If the local CLIs are already signed in to their own subscriptions, you can run real models without a separate API key: Codex (`codex`, ChatGPT subscription), Claude Code (`claude`, claude.ai subscription), Grok (`grok`, grok.com subscription), and Antigravity (`agy`, Google account).

```bash
cargo run --manifest-path rust/Cargo.toml -p council-core -- \
  --provider subscription
```

Pick the participating AIs with `--agents` (default `gpt,claude`; the seating order always follows the global fixed order GPT → Claude → Gemini → Grok).

```bash
cargo run --manifest-path rust/Cargo.toml -p council-core -- \
  --provider subscription --agents gpt,claude,gemini,grok
```

To check logins and executables without calling any model or spending subscription quota:

```bash
cargo run --manifest-path rust/Cargo.toml -p council-core -- \
  --provider subscription \
  --check-providers
```

On startup this mode inspects how each participating CLI is authenticated, and strips `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, and external cloud provider variables from the child processes. No provider session is stored; every judgement starts over in an ephemeral / no-session-persistence process built from the full room snapshot.

Optional model and effort selection (only where the CLI supports it):

```bash
export CODEX_SUBSCRIPTION_MODEL="..."                      # omit for the Codex subscription default
export CODEX_SUBSCRIPTION_EFFORT="high"                    # passed as model_reasoning_effort
export CLAUDE_SUBSCRIPTION_MODEL="sonnet"                  # default (no effort concept)
export GEMINI_SUBSCRIPTION_MODEL="gemini-3.7-flash-high"   # default
export GEMINI_SUBSCRIPTION_EFFORT="high"                   # agy --effort
export GROK_SUBSCRIPTION_MODEL="..."                       # omit for the Grok CLI default
export GROK_SUBSCRIPTION_EFFORT="high"                     # grok --reasoning-effort
```

This route **avoids separate pay-as-you-go API calls, but it is not unlimited.** Codex consumes your ChatGPT plan quota, and `claude -p` consumes either the separate monthly Agent SDK credit currently offered to subscribers or the applicable subscription quota. The Claude-side credit may require a separate opt-in on your account. To be sure spending stops when the quota or credit runs out, disable extra usage credits and auto-reload on the account. Also note that launching a CLI per inference is slower than the API — this is a local spike, not a deployable backend.

## Run real GPT + Claude — paid

Creating an API key is not what costs money; **calling models with the key is**. A ChatGPT subscription and OpenAI API billing are separate, as are a Claude subscription and Claude API billing. This spike never calls a real API unless you pass `--provider live` explicitly.

```bash
export OPENAI_API_KEY="..."
export ANTHROPIC_API_KEY="..."
cargo run --manifest-path rust/Cargo.toml -p council-core -- --provider live
```

The models can optionally be swapped:

```bash
export OPENAI_MODEL="gpt-5.4-mini"
export ANTHROPIC_MODEL="claude-sonnet-4-6"
```

As published on 2026-08-21, the default models cost, per million input/output tokens, `$0.75 / $4.50` for GPT-5.4 mini and `$3 / $15` for Claude Sonnet 4.6. Prices change, so re-check the [OpenAI pricing page](https://platform.openai.com/pricing) and the [Claude model pricing announcement](https://www.anthropic.com/news/claude-sonnet-4-6) before running.

A single human utterance triggers at least two model judgements. If the AIs hit the experiment's ceiling of three consecutive turns, that is `2 evaluations + 3 speeches + 3 re-evaluations = up to 8 API calls`. The entire event log is resent every time, so input tokens grow with the conversation.

## How it works

```text
commit RoomEvent
  → every participating AI processes the immutable room snapshot
  → listening barrier (everyone's last_heard_event advances)
  → PASS / REQUEST_FLOOR
  → if anyone requested, round-robin grants the floor
  → exactly one AI speaks
  → commit the new RoomEvent + discard all prior intents
  → process the new event again
```

- `Room.event_log` and `AgentState` are the only source of truth.
- Neither the OpenAI Responses API response id nor a Claude provider session is retained.
- A human event calls every participating AI in parallel.
- On an AI event the author is sync-only; the other AIs run a fresh inference.
- If any provider fails during the barrier, the cycle fails closed and nobody gets the floor.
- Simultaneous requests are resolved by round-robin over the fixed order alone — content, confidence, and latency are never consulted.
- If requesters remain after three AI turns, every last event is still processed and the cycle stops with `AI_STREAK_LIMIT`.

## Code boundaries

- `engine.rs` — room commit, barrier, intent invalidation, floor, streak limit
- `model.rs` — `RoomEvent`, `AgentState`, `Intent`
- `prompts.rs` — council rules, judgement and speaking instructions, the per-turn tool context
- `providers/mod.rs` — adapter construction and `seat_tools`, the single per-seat tool grant
- `providers/openai.rs` — OpenAI Responses API adapter
- `providers/anthropic.rs` — Claude Messages API adapter
- `providers/mock.rs` — free deterministic protocol demo
- `providers/subscription.rs` — ChatGPT / Claude / Grok / Antigravity subscription-login CLI adapters, with each seat's sandboxing
- `metrics.rs` — PASS rate, simultaneous REQUEST rate, AI streaks, human ratings
- `session.rs` — the JSON sidecar next to each transcript, Markdown import, atomic writes
- `review.rs` — cross-session aggregation, late ratings, exclusions
- `tests/protocol.rs` — core protocol invariants

## Verification

From the repository root:

```bash
scripts/fmt.sh --check
cd rust
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The tests cover both adapters reaching the parallel barrier, losing intents being discarded and re-judged, round-robin, fail-closed behaviour on provider failure, processing of the final AI event, metric computation, and transcript rendering. They are evidence about the orchestration, not about natural conversation. Judging real quality still requires live sessions on distinct topics plus human `/rate` scores.

## Web UI

There is a local web UI for actually using the thing. It sits outside the experiment contract as a usage layer, and it runs the same engine as the CLI.

```bash
cargo run --manifest-path rust/Cargo.toml -p council-core --bin council-web
```

The defaults are `--provider subscription --agents claude,gemini,grok --port 8787`. At http://127.0.0.1:8787 you get chat bubbles, the floor trace, metrics, and a 1–5 naturalness rating. AI utterances appear the moment they are committed, and the transcript is saved automatically to `outputs/web-session-<unix>.md`.

In the conversation view:

- **Deliberation progress** — each AI's judgement (thinking → PASS/REQUEST → composing) shows up as a live chip.
- **REQUEST_FLOOR reasons** — the internal reason given by a requesting AI is shown under the control trace with a `↳` and is kept in the transcript.
- **Directed utterances** — instead of addressing everyone, pick a specific AI and the first floor of that cycle goes to it (an extension of the "the human has priority" rule in EXPERIMENT.md §4).
- **Interrupt** — stop an in-flight deliberation. The cycle is recorded as `CANCELLED`, already-committed utterances are kept, and the round-robin cursor advances only for AIs that actually spoke.
- **History** — browse past sessions (JSON sidecars) and use "continue this conversation" to seed a new room **with the participant lineup exactly as it was stored**.
- **Turn hint** — when the room goes quiet, the composer says why: `QUIESCENT` means nobody asked for the floor and it is your turn; `AI_STREAK_LIMIT` and `CANCELLED` are named too.
- **Notifications** — a deliberation that finishes while the tab is unfocused posts a system notification and badges the tab title; permission is asked on your first send.
- **새 대화** (header) — starts a fresh room with the current lineup; the previous one is already saved.

In the settings panel (top right):

- **Per-seat subscription auth status** is checked and displayed (no model calls). Signing in itself happens in each CLI.
- **Participants, models, and effort** are chosen per seat, along with the AI consecutive-turn limit; **Start new session** applies them (the previous conversation is kept as a transcript and the room is reset).
- **Session budget** — set a cost ($) or token ceiling and new utterances are refused once it is reached (0 = unlimited).
- **This session's usage** — calls, input/output tokens, and CLI-reported cost, accumulated per AI. Remaining subscription quota is not shown because the CLIs do not expose it headlessly — hitting a limit surfaces as a fail-closed error, and the error text carries the reset time.
- **Speech language** — 자동 / 한국어 / English / 日本語 / 中文, applied on the next session; the AIs answer in it whatever language you type.
- **Workspace** — an absolute path the seats may read during speaking turns; see the v2 section below.

### Review board

The **리뷰** button opens the board that EXPERIMENT.md §6 is judged on: every saved session in `outputs/` in one table (topic, roster, PASS rate, simultaneous REQUEST rate, streaks, rating) under totals that recombine the counts rather than average the per-session rates. Each row takes a 1–5 naturalness score with an optional note, and an include toggle that holds a session out of the totals — use it for a contaminated session, such as an AI answering in its coding-agent persona. Mock sessions never count. The session still running is shown but locked until it ends. Ratings land in the same list the CLI's `/rate` writes to, so a topic has one rating history wherever it was scored. Markdown transcripts without a JSON sidecar are imported once at startup, so CLI sessions appear on the board too.

### Keeping the server up

The server lives as long as its process. To keep it running after the terminal closes:

```bash
nohup cargo run --manifest-path rust/Cargo.toml -p council-core --bin council-web > /dev/null 2>&1 &
```

## Working council (v2): tools on speaking turns

EXPERIMENT.md §7 extends the contract. Judgement turns stay tool-free, but a speaking turn in a *tool room* may search the web, read the room's folders, and — only on the human's explicit request — create files or run code, confined to a per-session artifacts folder. The grant is per seat, limited to what each CLI can confine mechanically, and defined once in `providers::seat_tools`; the prompt and the CLI arguments both read it, so a seat is never told about a tool it does not have (or denied one it has).

| Seat | Web | Read | Create files | Run code | Confinement |
| --- | --- | --- | --- | --- | --- |
| GPT (Codex) | ○ | ○ — whole disk; reads cannot be confined, so a rule is all that limits them | ○ | ○ | writes and execution sandboxed to the artifacts folder |
| Claude | ○ | ○ | only without a workspace | × | `--restricted` draws one combined read+write boundary; with a workspace set, Write is withheld so the workspace stays read-only by construction |
| Gemini (Antigravity) | ○ | × | × | × | no mechanical confinement exists, so no file tools are granted |
| Grok | ○ | ○ | ○ | ○ | kernel sandbox: workspace read-only, artifacts writable |

The web UI turns tools on for every session (artifacts under `outputs/artifacts/<session>/`); set a read-only **workspace** in the settings panel to let seats read your files. The CLI needs `--tools` (or `--workspace PATH`, which implies it); `--language ko|en|ja|zh` fixes the speech language in both.

Know before you use it: a tool speaking turn can take one to three minutes and spends noticeably more subscription quota than a plain turn (its timeout is 420s instead of 180s); the Codex seat can read anywhere on disk in a tool room; and the artifacts folder must not sit under a temp directory, because Grok's base sandbox keeps /tmp writable — the CLI refuses to start tools there. As of 2026-09-15 no saved session has exercised file creation or code execution; the only tool run so far is a web-search smoke test.

## Verification status (2026-09-15)

| Item | Status |
| --- | --- |
| fmt / clippy (-D warnings) / 56 tests | Passing |
| Full mock loop + transcript export | Passing |
| Subscription auth pre-check (`--check-providers`) | Passing — probes retry up to three times on a transient CLI failure |
| GPT subscription adapter, real judgement | Passed in a 2026-08-21 Codex session |
| Claude subscription adapter evaluate/speak | Passed on 2026-08-21 with real CLI calls (`structured_output` judgement + natural-language speech) |
| Barrier fail-closed (real provider error) | Passing — confirmed no floor granted when GPT hit its quota |
| Four-seat GPT + Claude + Gemini + Grok sessions | 10 topics run on 2026-08-28 |
| Review board over the saved sessions | 18 real sessions, 221 judgements aggregated (PASS 47.5%, simultaneous REQUEST 42.7%) |
| Conversation-quality acceptance (§6: 10 topics rated) | **Pending** — 5 of 10 rated so far (average 4.2). Three sessions with a known coding-agent persona leak are not yet excluded, so the totals above are contaminated |
| Rule 8 (let the human react first) and the v2 tool grants | **Unobserved** — added 2026-08-29; no saved session has run under them yet |
