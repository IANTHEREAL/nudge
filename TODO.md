# Nudge — TODO

## Phase 0 — Protocol Alignment (Day 1)

- [ ] Define Hub ↔ Daemon communication protocol (interfaces + example req/res)
- [ ] Define core data structure schemas
- [ ] Decide on agent suspension mechanism (tokio / goroutine / thread)
- [ ] Create Hub mock (for Daemon dev) and Daemon mock (for Hub dev)

## Phase 1 — Core Skeleton (Day 2–5)

### Hub Service Core (Owner: A)

- [ ] Subscription manager (CRUD + match inbound events by source/filter)
- [ ] Event queue (per-agent FIFO, in-memory, pending → delivered → acked)
- [ ] Timer Source Adapter (parse duration, fire event on expiry)
- [ ] Notification mechanism (notify Daemon when queue is non-empty)
- [ ] Unit tests: subscription matching, queue state transitions, Timer firing

### Daemon + Agent Skill API (Owner: B)

- [ ] Agent singleton manager (register / destroy / uniqueness guarantee)
- [ ] Agent state machine (Init → Sleeping → Running → Sleeping / Terminated)
- [ ] watch / read / ack API implementation
- [ ] Wake-up scheduler (receive notify → wake agent → batch pull → ordering)
- [ ] Unit tests: singleton constraint, state transitions, batch delivery ordering

## Phase 2 — Integration Testing (Day 6–7)

- [ ] Hub ↔ Daemon real integration (replace mocks, handle serialization / errors / timeouts)
- [ ] E2E: `watch calendar "5s"` → fire → wake → read → ack → watch again
- [ ] E2E: multiple timers batch fire → single read returns multiple events

## Phase 3 — GitHub + Persistence (Day 8–12)

### GitHub Source Adapter (Owner: A)

- [ ] Webhook reception and signature verification
- [ ] Parse PR / Workflow Run payloads
- [ ] Poll fallback + deduplication
- [ ] Filter matching (repo + event_type + number/run_id)
- [ ] Integration tests: real webhook triggers

### Persistence & Reliability (Owner: B)

- [ ] Event queue persistence (SQLite / Redis Stream)
- [ ] Daemon restart recovery (pull pending events, restore agent state)
- [ ] Event retry and dead letter queue
- [ ] Integration tests: crash recovery, retry flow

## Phase 4 — Integration + Wrap-up (Day 13–15)

- [ ] GitHub E2E (watch PR → merge → wake; watch run → complete → wake)
- [ ] Edge cases (network interruption recovery, processing timeout, duplicate dedup)
- [ ] Logging and metrics (queue depth, event latency, wake-up count)
- [ ] Documentation (API docs, Adapter development guide, operations manual)
