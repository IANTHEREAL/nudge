# Nudge — TODO

## Day 1: Core + Timer

- [ ] SQLite schema + subscription CRUD
- [ ] `nudge wait timer <duration>` — register + block + unblock on expiry
- [ ] `nudge on timer <duration> --run "..."` — register + dispatch on expiry
- [ ] `nudge list` / `nudge cancel`
- [ ] Daemon auto-start (background process with PID file)
- [ ] Tests: timer fires, wait unblocks, on dispatches, cancel works

## Day 2: GitHub Source

- [ ] GitHub condition checker (`gh api` calls)
- [ ] `nudge wait github pr <n> merged`
- [ ] `nudge wait github ci <n> success`
- [ ] `nudge wait github issue <n> new-comment`
- [ ] Batched polling (group by repo)
- [ ] Tests: PR merged, CI status, comment detected

## Day 3: Persistence + Reliability

- [ ] Daemon restart recovery
- [ ] Expired subscription cleanup
- [ ] `--timeout` handling
- [ ] `nudge status` command

## Day 4: Integration + Hardening

- [ ] `$NUDGE_EVENT` env var for callbacks
- [ ] Retry logic for `--run` failures
- [ ] Logging
- [ ] Edge cases (crash during dispatch, duplicate events)

## Day 5: Webhook + Polish

- [ ] Optional HTTP listener
- [ ] GitHub webhook signature verification
- [ ] `nudge wait webhook <path>`
- [ ] CLI help and error messages

## Day 6: Documentation + E2E

- [ ] README
- [ ] E2E: create PR → watch CI → gate review → watch merge → done
- [ ] Package and release
