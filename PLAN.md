# Nudge — Project Plan

## 1. What is Nudge

Nudge is an event-driven agent coordination system. It allows agents to declaratively listen to external event sources (GitHub, Calendar, etc.), get "nudged" awake when events fire, and process events in order.

Typical usage:

```
nudge watch github <repo> pr 42          # Watch for PR status changes
nudge watch github <repo> run 114514     # Watch for CI run results
nudge watch calendar "1 hour"            # Wake up after 1 hour
```

---

## 2. Architecture

```
┌──────────────────────────────────────────────────────┐
│                     Agent Skill                      │
│              watch() / read() / ack()                │
│                   ▲          │                       │
│                   │    All API calls                 │
│                   │    go through Daemon             │
└──────────────────────────────────────────────────────┘
                    │          │
                    ▼          ▼
┌──────────────────────────────────────────────────────┐
│                      Daemon                          │
│                                                      │
│   ┌──────────────────────────────────────────────┐   │
│   │  Agent Singleton Mgmt    State Machine       │   │
│   │  Init → Sleeping → Running → Sleeping / End  │   │
│   └──────────────────────────────────────────────┘   │
│                    │          ▲                       │
│          subscribe │          │ notify(agent_id)     │
│                    ▼          │                       │
└──────────────────────────────────────────────────────┘
                    │          │
                    ▼          │
┌──────────────────────────────────────────────────────┐
│                   Hub Service                        │
│                                                      │
│   ┌────────────┐  ┌────────────┐  ┌──────────────┐  │
│   │ Subscription│  │ Event      │  │ Source       │  │
│   │ Manager    │  │ Queue      │  │ Adapters     │  │
│   │            │  │ (per agent) │  │ ┌──────────┐ │  │
│   └────────────┘  └────────────┘  │ │ GitHub   │ │  │
│                                   │ │ Timer    │ │  │
│                                   │ │ Webhook  │ │  │
│                                   │ └──────────┘ │  │
│                                   └──────────────┘  │
└──────────────────────────────────────────────────────┘
```

The three layers each have their own responsibilities:

- **Hub Service** — Centrally manages event source subscriptions and polling; maintains an ordered event queue per agent.
- **Daemon** — Manages agent singleton lifecycle (start, suspend, wake, destroy); the sole bridge between agents and the Hub.
- **Agent Skill** — Agents interact with the event system via watch / read APIs; all requests are proxied through the Daemon.

### Core Constraints

- **Singleton**: Only one instance of a given agent exists within each Daemon.
- **Sequential Processing**: Events are delivered to agents in arrival order; no concurrent dispatch.
- **Batch Delivery**: On wake-up, all pending events in the queue are delivered to the agent at once.

### Agent Lifecycle

```
                 watch()
  ┌──────┐     ┌──────────┐    event arrived     ┌─────────┐
  │ Init │────►│ Sleeping  │───────────────────►│ Running  │
  └──────┘     └──────────┘                     └────┬─────┘
                     ▲                                │
                     │    processing done / watch again│
                     └────────────────────────────────┘
                                                      │
                                                      │ exit
                                                      ▼
                                                ┌───────────┐
                                                │Terminated │
                                                └───────────┘
```

### Key Data Structures

```
Subscription {
    id:         string
    agent_id:   string
    source:     string        // "github" | "calendar" | "webhook"
    filter:     SourceFilter  // source-specific
    created_at: timestamp
}

Event {
    id:         string
    source:     string
    payload:    any
    matched:    Subscription
    timestamp:  timestamp
    status:     "pending" | "delivered" | "acked"
}
```

### Daemon API (called by Agent Skill)

```
POST /watch   { source, filter }       → Register listener, agent suspends
GET  /read                             → Pull triggered events for current batch
POST /ack     { event_ids }            → Acknowledge events as processed
```

### Hub ↔ Daemon Internal Protocol

```
subscribe(agent_id, source, filter) → subscription_id
unsubscribe(subscription_id)
pull_events(agent_id)               → Vec<Event>
ack_events(event_ids)
notify(agent_id)                     // Hub → Daemon wake-up notification
```

---

## 3. Source Adapters

Each event source implements a unified interface:

```
trait SourceAdapter {
    fn subscribe(filter: SourceFilter) -> SubscriptionHandle
    fn unsubscribe(handle: SubscriptionHandle)
    fn poll() -> Vec<RawEvent>
    fn handle_webhook(req: Request)
}
```

Initially supported:

| Source | Mode | Filter Example |
|--------|------|----------------|
| GitHub | Webhook + Poll fallback | `{ repo, event_type: "pull_request", number: 42 }` |
| Timer  | Internal timer | `{ duration: "1h" }` or `{ cron: "0 9 * * *" }` |
| Webhook (Generic) | Webhook | `{ path: "/hook/xxx", jmespath: "..." }` |

---

## 4. End-to-End Event Flow

```
 1. Agent calls watch(github, { repo: "foo/bar", pr: 42 })
 2. Daemon forwards subscription → Hub Service registers it
 3. Agent enters Sleeping state, Daemon suspends
 4. Hub detects PR #42 change via webhook/poll
 5. Hub generates Event, writes it to the agent's queue
 6. Hub calls notify(agent_id) to alert the Daemon
 7. Daemon wakes the agent
 8. Agent calls read() → receives [evt_001, evt_002, ...]
 9. Agent processes events
10. Agent calls ack(event_ids)
11. Agent decides: watch again → go to step 3 | exit → Terminated
```

---

## 5. Division of Work

Two people split by **layer**, interface-first, parallel development:

| | Owner | Scope | Core Responsibilities |
|---|---|---|---|
| Hub Side | **A** | Hub Service + Source Adapters | Subscription management, event queue, source adapters (Timer / GitHub / Webhook), event matching & delivery |
| Daemon Side | **B** | Daemon + Agent Skill API | Agent singleton lifecycle, state machine, watch/read/ack API, wake-up scheduling, persistence & reliability |

Why split by layer:

1. The Hub ↔ Daemon boundary is clean — once the protocol is defined, both sides can mock the other and develop in parallel without blocking each other.
2. Each person has full ownership of their layer, reducing merge conflicts.
3. Integration is concentrated at two points (Phase 2 and Phase 4); the rest of the time both sides progress independently.

---

## 6. Development Phases

### Phase 0 — Protocol Alignment (Day 1)

> A + B work together. Deliverables: `protocol.md` + mocks on both sides.

- [ ] Define Hub ↔ Daemon communication protocol (interfaces + example request/response) `A+B`
- [ ] Define core data structure schemas `A+B`
- [ ] Decide on agent suspension mechanism (tokio / goroutine / thread) `A+B`
- [ ] A provides Hub mock for B; B provides Daemon mock for A `A+B`

---

### Phase 1 — Core Skeleton, Parallel Development (Day 2–5)

**A — Hub Service Core**

- [ ] Subscription manager (CRUD + match inbound events by source/filter) `A`
- [ ] Event queue (per-agent FIFO, in-memory, pending → delivered → acked) `A`
- [ ] Timer Source Adapter (parse duration, fire event on expiry) `A`
- [ ] Notification mechanism (notify Daemon mock when queue is non-empty) `A`
- [ ] Unit tests (subscription matching, queue state transitions, Timer firing) `A`

**B — Daemon + Agent Skill API**

- [ ] Agent singleton manager (register / destroy / uniqueness guarantee) `B`
- [ ] Agent state machine (Init → Sleeping → Running → Sleeping / Terminated) `B`
- [ ] watch / read / ack API implementation `B`
- [ ] Wake-up scheduler (receive notify → wake agent → batch pull → ordering guarantee) `B`
- [ ] Unit tests (singleton constraint, state transitions, batch delivery ordering) `B`

---

### Phase 2 — Integration Testing (Day 6–7)

> Remove mocks, real integration, run through the Timer scenario end-to-end.

- [ ] Hub ↔ Daemon real integration (replace mocks, handle serialization / errors / timeouts) `A+B`
- [ ] E2E: `watch calendar "5s"` → fire → wake → read → ack → watch again `A+B`
- [ ] E2E: Multiple timers batch fire → single read returns multiple events `A+B`

---

### Phase 3 — GitHub + Persistence, Parallel Development (Day 8–12)

**A — GitHub Source Adapter**

- [ ] Webhook reception and signature verification `A`
- [ ] Parse PR / Workflow Run payloads `A`
- [ ] Poll fallback + deduplication `A`
- [ ] Filter matching (repo + event_type + number/run_id) `A`
- [ ] Integration tests (real webhook triggers) `A`

**B — Persistence & Reliability**

- [ ] Event queue persistence (SQLite / Redis Stream) `B`
- [ ] Daemon restart recovery (pull pending events, restore agent state) `B`
- [ ] Event retry and dead letter queue `B`
- [ ] Integration tests (crash recovery, retry flow) `B`

---

### Phase 4 — Integration + Wrap-up (Day 13–15)

- [ ] GitHub E2E (watch PR → merge → wake; watch run → complete → wake) `A+B`
- [ ] Edge cases (network interruption recovery, processing timeout, duplicate event dedup) `A+B`
- [ ] Logging and metrics (queue depth, event latency, wake-up count) `A+B`
- [ ] Documentation (API docs, Adapter development guide, operations manual) `A+B`

---

## 7. Dependencies & Timeline

```
Day 1       Phase 0 ─── Protocol Alignment ─── A+B
            ───────────────────────────────────────
Day 2–5     Phase 1     A: Hub Core + Timer
                        B: Daemon + Agent API     (parallel)
            ───────────────────────────────────────
Day 6–7     Phase 2 ─── Integration Testing ─── A+B
            ───────────────────────────────────────
Day 8–12    Phase 3     A: GitHub Adapter
                        B: Persistence + Reliability  (parallel)
            ───────────────────────────────────────
Day 13–15   Phase 4 ─── Integration + Wrap-up ─── A+B
```

Out of 15 days, A and B block each other for ≤ 4 days (Phase 0 / 2 / 4); the rest can be progressed independently.

---

## 8. Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Insufficient protocol definition in Phase 0 → heavy rework in Phase 2 | High | Protocol docs must include example req/res; cross-review by both sides |
| GitHub Webhook environment dependency → inconvenient local development | Medium | A provides a webhook replay tool to replay recorded payloads |
| Persistence changes affect Hub internal interfaces | Medium | Hub in Phase 1 should include a storage abstraction layer; B aligns with A early |
| Delayed decision on agent suspension mechanism | Medium | Must be decided in Phase 0; do not defer to Phase 1 |

---

## 9. Open Questions

1. **Multi-watch semantics** — When an agent watches multiple sources simultaneously, is the wake condition ANY or ALL? Suggestion: default to ANY.
2. **Watch cancellation** — Is an explicit unwatch API needed? Or is it implicitly cancelled by exit / re-watch?
3. **Event idempotency** — Does the Hub guarantee exactly-once, or does the agent handle its own dedup?
4. **Timeout mechanism** — Maximum wait time for a sleeping agent? Prevent indefinite suspension.
5. **Multi-Daemon instances** — How to coordinate during future scaling (distributed lock / leader election)?
