# Nudge — Subscription System for AI Agents

## 1. What is Nudge

Nudge lets agents declare what they're waiting for, then get woken up when it happens.

```bash
nudge wait github pr 42 merged         # block until PR 42 is merged
nudge wait ci 2199 success             # block until CI passes
nudge wait timer 30m                   # block for 30 minutes
nudge wait issue 2208 new-comment      # block until new comment appears
```

For cross-session use (agent exits, event fires later, new agent picks up):

```bash
nudge on github pr 42 merged --run "claude -p 'PR 42 merged. Deploy to staging.'"
nudge on ci 2199 success --run "claude -p 'CI passed. Run gate review on PR 2199.'"
nudge on issue 2208 new-comment --run "claude -p 'New comment on #2208. Re-evaluate design.'"
```

## 2. Core Concept

**Nudge is a subscription system, not a notification system.**

An agent declares "I care about X." How the notification arrives (polling, webhook, GitHub Actions) is an implementation detail that can change per-source without affecting the agent's code.

```
┌─────────────────────────────────────┐
│            Agent                     │
│                                      │
│   nudge wait <source> <condition>    │  ← one command, blocks
│   nudge on <source> <cond> --run ... │  ← one command, exits, callback later
│                                      │
└──────────────────┬───────────────────┘
                   │
                   ▼
┌──────────────────────────────────────┐
│         Subscription Store           │
│                                      │
│   { source, condition, callback,     │
│     created_at, expires_at, status } │
│                                      │
│   Storage: SQLite file               │
└──────────────────┬───────────────────┘
                   │
                   ▼
┌──────────────────────────────────────┐
│         Condition Checker            │
│                                      │
│   Polling loop (configurable freq)   │
│   + optional webhook fast-path       │
│                                      │
│   ┌──────────┐ ┌────────┐ ┌───────┐ │
│   │ GitHub   │ │ Timer  │ │Webhook│ │
│   │ (gh CLI) │ │(clock) │ │(HTTP) │ │
│   └──────────┘ └────────┘ └───────┘ │
└──────────────────┬───────────────────┘
                   │ condition met
                   ▼
┌──────────────────────────────────────┐
│          Dispatcher                  │
│                                      │
│   wait mode: unblock the caller      │
│   on mode:   execute --run command   │
│              (claude -p, curl, etc.) │
└──────────────────────────────────────┘
```

Two components, one process. Not three layers.

## 3. Two Modes

### `nudge wait` — Synchronous (agent stays alive)

Agent blocks until the condition is met. Returns the event as JSON to stdout.

```bash
EVENT=$(nudge wait github pr 42 merged --timeout 2h)
echo $EVENT  # {"source":"github","type":"pr_merged","pr":42,"merged_at":"..."}
```

Use when: short waits (CI completion, gate review result), agent session stays alive.

### `nudge on` — Asynchronous (agent exits, callback later)

Registers a persistent subscription and exits immediately. When the condition fires, nudge executes the `--run` command (typically spawning a new agent session).

```bash
nudge on github pr 42 merged --run "claude -p 'PR 42 merged. Start deployment.'"
nudge on timer 6h --run "claude -p 'Grace period expired. Revoke old password.'"
```

Use when: long waits (human response, grace period expiry), agent session doesn't need to stay alive.

## 4. Subscription Types

| Source | Conditions | Check Method |
|--------|-----------|--------------|
| **github pr** | `merged`, `closed`, `review-approved`, `label:<name>`, `new-comment`, `ci-passed` | `gh api` polling (default 60s) |
| **github issue** | `new-comment`, `closed`, `label:<name>` | `gh api` polling |
| **github ci** | `success`, `failure`, `completed` | `gh api` polling |
| **timer** | duration (`30m`, `6h`, `7d`) | internal clock |
| **webhook** | path match + optional jmespath filter | HTTP listener (opt-in) |

GitHub sources use `gh` CLI — no token management, no API client library. Polling is the default; webhook is an optimization added later.

## 5. Data Model

One table, minimal:

```sql
CREATE TABLE subscriptions (
    id          TEXT PRIMARY KEY,
    source      TEXT NOT NULL,      -- "github", "timer", "webhook"
    condition   TEXT NOT NULL,       -- JSON: {"repo":"foo/bar","pr":42,"event":"merged"}
    mode        TEXT NOT NULL,       -- "wait" or "on"
    callback    TEXT,                -- for "on" mode: the --run command
    status      TEXT DEFAULT 'active', -- active | fired | expired | cancelled
    created_at  INTEGER NOT NULL,   -- epoch seconds
    expires_at  INTEGER,            -- epoch seconds, NULL = no expiry
    event_data  TEXT                -- JSON payload when fired
);
```

SQLite file at `~/.nudge/subscriptions.db`. Survives process restarts, agent session restarts, machine reboots.

## 6. Architecture: Single Process

```
nudge daemon
  ├── Subscription Store (SQLite)
  ├── Polling Loop
  │   ├── GitHub checker (gh api calls, batched per-repo)
  │   ├── Timer checker (compare expires_at vs now)
  │   └── Webhook listener (optional, opt-in via --enable-webhooks)
  ├── Dispatcher
  │   ├── wait mode: write to named pipe / Unix socket → unblock caller
  │   └── on mode: spawn --run command
  └── CLI interface
      ├── nudge wait ...    (register + block)
      ├── nudge on ...      (register + exit)
      ├── nudge list        (show active subscriptions)
      ├── nudge cancel <id> (cancel subscription)
      └── nudge daemon      (start the background daemon)
```

**Why single process:** Agent coordination systems with < 100 subscriptions don't need distributed architecture. A single daemon with SQLite handles thousands of subscriptions trivially. If scaling is ever needed (it won't be), the subscription store can be swapped to PostgreSQL without changing the API.

**Daemon auto-start:** `nudge wait` and `nudge on` auto-start the daemon if not running (like Docker daemon). No separate startup step needed.

## 7. Polling Strategy

Not all subscriptions need the same frequency:

| Source | Default Poll Interval | Rationale |
|--------|----------------------|-----------|
| github ci | 30s | CI results are time-sensitive |
| github pr | 60s | PR events are less urgent |
| github issue | 120s | Comment watches are low-urgency |
| timer | N/A | Internal clock, no polling |
| webhook | N/A | Push-based, no polling |

**Batching:** Multiple subscriptions for the same repo are batched into one `gh api` call. Watching 10 PRs on the same repo costs 1 API call, not 10.

**Rate limiting:** GitHub API rate limit is 5000/hour. At 60s intervals, 1 repo = 60 calls/hour. Can watch ~80 repos simultaneously before hitting limits.

**Webhook fast-path (optional):** If `--enable-webhooks` is set, nudge starts an HTTP listener. GitHub webhooks provide near-instant delivery. Polling continues as fallback (webhooks can be missed).

## 8. Agent Integration

### With Claude Code Skills

```bash
# In a skill: after creating PR, wait for CI to pass before gate review
gh pr create --repo $REPO --head $BRANCH --title "..." --body "..."
EVENT=$(nudge wait ci $PR_NUMBER success --timeout 30m)
if [ $? -eq 0 ]; then
  /gate-review $PR_NUMBER
fi
```

### With Harness

The harness dispatcher can use `nudge on` instead of cron polling:

```bash
# Instead of cron polling labels every 5 minutes:
nudge on github pr $PR_NUMBER label:harness:review-done \
  --run "claude -p '/harness continue $ISSUE --repo $REPO'"
```

### Cross-Session State

`nudge on` callbacks receive event data via `$NUDGE_EVENT` environment variable:

```bash
nudge on github issue 2208 new-comment \
  --run "claude -p 'New comment on #2208. Event: \$NUDGE_EVENT'"
```

## 9. CLI Reference

```
nudge wait <source> <args> [--timeout <duration>]
  Block until condition met. Print event JSON to stdout.
  Exit 0 on success, 1 on timeout, 2 on error.

nudge on <source> <args> --run "<command>" [--timeout <duration>]
  Register persistent subscription. Exit 0 immediately.
  When fired, execute <command> with $NUDGE_EVENT set.

nudge list [--source <source>] [--status <status>]
  List subscriptions.

nudge cancel <id>
  Cancel a subscription.

nudge daemon [--enable-webhooks] [--webhook-port 9876]
  Start daemon (auto-started by wait/on if not running).

nudge status
  Show daemon status, subscription count, last poll times.
```

## 10. Implementation Plan (6 days, 1 person)

### Day 1: Core + Timer

- [ ] SQLite schema + subscription CRUD
- [ ] `nudge wait timer <duration>` — register + block + unblock on expiry
- [ ] `nudge on timer <duration> --run "..."` — register + dispatch on expiry
- [ ] `nudge list` / `nudge cancel`
- [ ] Daemon auto-start (background process with PID file)
- [ ] Tests: timer fires, wait unblocks, on dispatches, cancel works

### Day 2: GitHub Source

- [ ] GitHub condition checker (`gh api` calls, parse PR/issue/CI state)
- [ ] `nudge wait github pr <n> merged` — poll until condition, unblock
- [ ] `nudge wait github ci <n> success` — poll until CI green
- [ ] `nudge wait github issue <n> new-comment` — track comment count
- [ ] Batched polling (group by repo)
- [ ] Tests: PR merged detected, CI status detected, comment detected

### Day 3: Persistence + Reliability

- [ ] Daemon restart recovery (reload active subscriptions from SQLite)
- [ ] Expired subscription cleanup
- [ ] `--timeout` handling (wait returns exit code 1)
- [ ] Concurrent subscription stress test
- [ ] `nudge status` command

### Day 4: Integration + Hardening

- [ ] `$NUDGE_EVENT` environment variable for `--run` callbacks
- [ ] Retry logic for `--run` command failures
- [ ] Logging (subscription lifecycle, poll results, dispatch events)
- [ ] Edge cases: daemon crash during dispatch, duplicate events

### Day 5: Webhook + Polish

- [ ] Optional HTTP listener for webhook fast-path
- [ ] GitHub webhook signature verification
- [ ] `nudge wait webhook <path>` for generic webhooks
- [ ] CLI help text and error messages

### Day 6: Documentation + E2E Test

- [ ] README with quick start
- [ ] End-to-end: create PR → watch CI → gate review → watch merge → done
- [ ] Package and release

## 11. What This Design Does NOT Include

Intentionally excluded:

- **No Hub/Daemon separation** — single process is enough
- **No event queue or ack semantics** — agents process events inline
- **No agent lifecycle management** — the OS manages processes
- **No distributed coordination** — single-machine tool
- **No plugin/adapter trait** — new sources are added as code

These can be added if proven necessary by real usage.

## 12. Open Questions (Answered)

| Question | Answer |
|----------|--------|
| Multi-watch (ANY or ALL)? | `nudge wait-any` for ANY. ALL is composition: chain subscriptions. |
| Watch cancellation? | `nudge cancel <id>`. Also cancelled on timeout or daemon shutdown. |
| Event idempotency? | Dedup via subscription status (fired → ignore). |
| Timeout? | `--timeout <duration>` on every command. Default: no timeout for `on`, 1h for `wait`. |
| Scaling? | Not needed. Single daemon handles thousands of subscriptions. |
| Language? | Go or Rust. Single binary, no runtime dependencies. Decision made before Day 1, not during. |
