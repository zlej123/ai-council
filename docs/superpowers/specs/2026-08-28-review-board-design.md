# Review board — design

## Problem

EXPERIMENT.md §6 decides conversation quality by reading **at least ten different
topics together**, weighing naturalness average, PASS rate, simultaneous REQUEST
rate, and the AI streak distribution side by side. The web UI cannot support that
today:

- The history drawer lists only date and event count.
- The session viewer shows the conversation and nothing else.
- Metrics live on the *running* session and disappear on reset.
- `/api/rate` only rates the live session, so a past topic can never be scored.
- A contaminated session (e.g. `outputs/live-claude-gemini-grok-3.md`, where Claude
  answered in a coding-agent persona) cannot be marked or held out of the totals.

The data is already there: every `outputs/*.json` sidecar stores `roster`, `events`,
`cycles.barriers`, and the full `metrics` report including `naturalness_ratings`.

## Scope

A read-mostly review layer over the sidecars. It is part of the usage layer, not the
experiment contract: the engine, the protocol, and `Room.event_log` as the source of
truth are untouched, so EXPERIMENT.md needs no change.

## Design

### `council_core::review` (new library module)

Pure functions over sidecar data, so the aggregation is unit-testable without a server.

- `SessionSummary` — file, saved_unix, provider, roster, topic (first human event,
  truncated), event count, `MetricsReport`, and a `ReviewAnnotation`.
- `ReviewAnnotation { excluded: bool, reason: Option<String> }`, defaulted so existing
  sidecars deserialize unchanged.
- `aggregate(&[SessionSummary]) -> ReviewAggregate` over the **included** sessions only.

Aggregation must combine counts, not average the per-session rates:

| Metric | Combination |
| --- | --- |
| PASS rate | `Σ round(pass_rate × decisions) / Σ decisions` |
| Simultaneous REQUEST rate | `Σ round(rate × multi_evaluation_barriers) / Σ barriers` |
| Streaks | merge `ai_streak_histogram`; recompute count, mean, and max from the merge |
| Naturalness | concatenate `naturalness_ratings`; average over all of them |

`MetricsReport` stores rates plus their denominators, so the numerators are recovered by
multiplying back and rounding. This keeps the sidecar format unchanged; a mean of means
would silently over-weight short sessions.

### `SessionRecord` moves into the library

The sidecar struct currently lives inside the `council-web` binary, which is why none of
this is testable. Move `SessionRecord` and its `UiEvent` / `UiCycle` / `UiBarrier` parts
into `council_core::session`; the binary imports them from there.

### Writes

Annotations and late ratings edit the sidecar in place, but never by round-tripping the
whole file through a typed struct — that would silently drop any key the struct does not
know. Read as `serde_json::Value`, replace only the touched key, write back:

- **Rate a past session** — deserialize `metrics` into `MetricsReport`, push the rating,
  recompute `naturalness_average`, put it back. Same home as the live `/rate`, so a topic
  has exactly one rating list wherever it was scored.
- **Exclude a session** — set the `review` key.

### HTTP

- `GET /api/review` → `{ sessions: [SessionSummary], aggregate: ReviewAggregate }`
- `POST /api/review/rate` → `{ file, score, note? }`
- `POST /api/review/exclude` → `{ file, excluded, reason? }`

All three reuse `safe_session_file` so a file name cannot escape `outputs/`.

### UI

A review panel listing one row per session — topic, roster, events, PASS rate,
simultaneous REQUEST rate, mean/max streak, rating, include toggle — above a totals
header carrying the four §6 numbers and a streak histogram, captioned "N of M included".
Rating buttons sit on the row; the row opens the existing viewer.

## Out of scope

Collapsing the control trace and tidying the metrics footer are a separate, smaller
change made after this one lands.
