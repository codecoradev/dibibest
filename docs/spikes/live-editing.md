# Spike: server-ordered live text editing (issue #88, DIB-10)

Status: spike, written 2026-11-25. Nothing here merges as is. **The spike code must never be mounted in a deployed server:** it has no
authentication (the actor is a query or body field and the route picks whether it is a person or an agent), creates rooms and projects
on first use without a limit, and its per-connection queue caps frames and bytes for durable and presence frames but not for the
server's own control frames (a client that floods bad frames makes the server queue a rejection for each one). All of that is listed
under "What the real build still needs"; it is repeated here because the routes are mounted by `live::routes()` on that branch. The code is on the branch `test/live-editing-spike` at commit `ead96c1` (not merged, no pull request; the stages mentioned below are
those of epic #84): `crates/dibibest-server/src/live.rs`
and `src/live/{text,frames,gate,room}.rs`, tests in `crates/dibibest-server/tests/live_spike.rs` (plus unit tests in
`live/text.rs` and `live/room.rs`).

**Question:** does server-ordered text editing (the model of `@codemirror/collab`, no CRDT) work well for the schema editor, with
durable numbered frames, ephemeral presence and a resync marker?

**Answer: yes, with changes.** See [Recommendation](#recommendation).

## What was built

One room actor per project (a tokio task) owns the text (UTF-16 units, as a browser string) and the order. A WebSocket route
`GET /v1/live/{project}?since_seq=N&actor=name` joins a connection to the room. A second entry point,
`POST /v1/live/{project}/agent-changes` (and the plain function `RoomHandle::submit`), submits a change as an agent. Both entry
points end in the same `Cmd::Submit` of the actor and the same `gate()` call.

The path of a change in the room (`Room::submit`):

1. If the idempotency key is one the room still remembers, answer `ack` with the old `seq`; write nothing.
2. Check `base_seq`: ahead of the room → `base_in_future`; older than the replay window → `too_old` (plus a `resync`).
3. Build the change on the text length at `base_seq` (bounds, order and overlap are checked: `bad_change`).
4. **Rebase** it over the ops accepted since `base_seq` (`text::transform`, accepted ops go first on ties).
5. Apply to a copy of the text, call the **gate** with the result.
6. `Store::append` (the log is the only source of `seq`). Only after it returns is the change the new text, and only then does anyone hear.
7. Fan out: everyone but the sender gets `op`; the sender gets `ack` at the same place of its stream.

Fan-out never waits. Every connection has a bounded queue (`ClientQueue`, default 1024 frames / 4 MiB). A durable frame that does
not fit empties the queue and marks the client; the writer task of that connection then asks the room for a `resync` (the whole
text at the current `seq`) and durable frames are dropped for that client until it is in. An ephemeral frame is dropped when the
queue is more than half full and never causes a resync.

### Server rebase or client rebase? Server rebase.

`@codemirror/collab` lets the authority either reject a change whose version is not current, or rebase and accept it. I read the
current text of <https://codemirror.net/examples/collab/> and the `@codemirror/collab` reference
(<https://codemirror.net/docs/ref/>: `collab`, `receiveUpdates`, `sendableUpdates`, `rebaseUpdates`, `getSyncedVersion`,
`getClientID`; fetched with `curl` on 2026-11-25, there was no `WebFetch` tool in this session). What they say, and what I relied on:

- The authority "can ... either reject the updates, or rebase and accept them". Reject-only "can in some circumstances lead to
  starvation — if a client has a higher latency than its peers, and those peers keep submitting changes, it may be unable to get
  its updates through for a long time". "It is recommended to rebase updates whenever practical." Rejecting a version older than
  the history you keep is explicitly fine.
- The **client** always rebases its unconfirmed changes over the ones it receives (`receiveUpdates`), whichever way the server
  chooses; the server-side rebase is only for a submit that arrives on an old version.
- `rebaseUpdates` also drops updates that were already accepted. That is CM's own dedupe; here the idempotency key does it.

So: the room rebases (the same retain/delete/insert transform as the client's), and rejects only outside the replay window. It
costs one transform per op accepted since the base (**not isolated by any measurement**: see Measurements) and avoids the starvation the docs warn about.
The test client implements the client half (`Client::apply`: transform pending changes over a received op) and the room and the
client reach the same text at every version, which is the property that matters.

## How each DIB-10 acceptance point was shown

| # | Point | Test(s) in `tests/live_spike.rs` |
|---|-------|------|
| 1 | Room actor per project, WS route, frame set | `a_change_is_acked_to_the_sender_and_broadcast_to_the_other`, `bad_changes_and_bad_frames_are_rejected_and_the_connection_lives`, `the_socket_works_behind_the_real_router_and_outlives_the_request_timeout` (whole router: rate limit, body limit, request timeout, hand-rolled accept loop with upgrades) |
| 2 | Server rebase, idempotency key | `a_change_made_on_an_old_version_is_rebased_by_the_room`, `an_unacked_submit_is_safe_to_send_again`; unit tests in `live/text.rs` (tie rule, insert inside a concurrent delete, overlapping deletes, UTF-16 positions) |
| 3 | Two clients, hundreds of changes, nothing lost | `two_clients_type_concurrently_and_every_marker_is_there_once` (2 × 300 changes, 484 of 600 had to be rebased by the room, up to 13 ops deep; both clients, a fresh client and an independent replay of the store's log show the same text; each of the 600 unique markers is in it exactly once). `concurrent_inserts_and_deletes_converge` (2 × 250 random inserts and deletes: same text everywhere) |
| 4 | Reconnect, replay, retry, resync | `a_client_that_reconnects_gets_the_missed_ops_in_order_once` (the client asserts every durable frame is the next number: no gap, no repeat), `an_unacked_submit_is_safe_to_send_again` (ack lost: the replay carries the client's own key and confirms it; a re-sent key is answered with the old `seq` and the log does not grow), `a_client_too_far_behind_gets_a_resync_with_the_whole_text` (window of 8; a `since_seq` the room never had; a submit on a base outside the window → `too_old` + `resync`) |
| 5 | Slow client | `a_client_that_does_not_read_gets_a_resync_and_slows_nobody`; unit tests of the queue in `live/room.rs` |
| 6 | Presence is ephemeral | `presence_is_live_only_never_stored_never_replayed`; queue unit tests `ephemeral_frames_are_dropped_early_and_never_cause_a_resync` |
| 7 | Agent write through the gate | `the_gate_refuses_an_agent_change_that_does_not_parse_and_lets_a_person_edit_mid_sentence`, `the_rest_like_route_goes_through_the_same_gate` |
| 8 | Restart | `a_restart_serves_the_same_text_and_the_same_numbers` (also: a replay from a pre-restart version works, because the window is rebuilt from the log) |
| – | Shutdown | `shutdown_with_an_open_socket_ends_and_tells_what_it_cost` (observes, see below) |

Run the measurements with
`cargo test --release -p dibibest-server --test live_spike -- --ignored --nocapture --test-threads=1`.

### The gate (point 7)

`live::gate::gate(actor, units, text)` is the only function that decides, and the room calls it for every change whatever it came in
through. A person: always accepted, unless the text would exceed a size limit (`MAX_TEXT_UNITS`, 1 Mi units, a placeholder for the quota).
An agent: the result is parsed as DBML by `dibibest-core` and refused if it does not parse, with the parser's message:
`the result is not valid DBML: parse error in dbml: line 2: bad column line`. In the test the identical change (`" [pk"` in a column)
is refused for the agent and accepted for the person over the socket; the next agent change, which mends the text (`"]"`), is accepted,
because the gate judges the *result*, not the intent. A refused change leaves the text and `seq` untouched. A person's text is never
parsed in the gate (the closure that builds the string is not called), so typing costs no parse.

## Measurements

Machine: macOS (Darwin 25.5), Apple silicon, loopback, one SQLite file in a tempdir with the real store (WAL, `synchronous=FULL`),
release build unless noted. Single runs; read them as orders of magnitude.

**Slow client** (120 changes, each replaces the whole text with about 240 KB of JSON, so about 29 MB were submitted to the room, and the same again went out to each reader; the test prints 480 because it counts UTF-16 bytes; the room's queue per
client is 64 frames / 1 MiB; the stuck client connected, read its hello and then stopped reading; a second reader reads normally).
Latency is what the writer measured from sending a change to its ack:

| run (release) | no stuck client p50 / p99 / max | with a stuck client p50 / p99 / max |
|---|---|---|
| 1 | 1.56 / 7.98 / 8.26 ms | 1.53 / 11.4 / 24.9 ms |
| 2 | 1.52 / 11.9 / 12.3 ms | 1.58 / 16.6 / 23.6 ms |
| 3 | 2.44 / 20.8 / 28.9 ms | 1.59 / 13.6 / 14.4 ms |
| 4 | 1.66 / 13.6 / 23.2 ms | 1.52 / 11.0 / 11.1 ms |
| debug build | 30.4 / 66.8 / 157.7 ms | 30.3 / 46.6 / 47.0 ms |

The stuck client changes nothing I can see: the spread between runs of the same case is bigger than the difference between the cases.
In every run the stuck client got exactly one `resync` once it started reading, and then showed the same text as the others. The
reader that read was never given a resync. The room never waited on a socket: it has no `await` on one.

**Throughput of one room** (small appends, each client sends one change and waits for its ack before the next; the store's
`append` is awaited by the actor, one at a time):

| clients | release, run A (the author's) | release, re-run by the reviewer | debug |
|---|---|---|---|
| 1 | 7.3k ops/s (ack p50 104 µs, p99 350 µs) | 6.3k ops/s (p50 121 µs, p99 506 µs) | 4.3k ops/s |
| 4 | 6.5k ops/s (p50 0.43 ms, p99 3.95 ms) | 5.2k ops/s (p50 0.39 ms, p99 6.9 ms) | 3.6k ops/s |
| 16 | 5.8k ops/s (p50 2.2 ms, p99 7.7 ms) | 6.3k ops/s (p50 2.1 ms, p99 5.2 ms) | 3.2k ops/s |

The two runs differ by up to about 20% and do not agree on the order across client counts: read it as "5 to 7 thousand per second on this
machine", nothing finer.

**Treat this as an upper bound.** macOS `fsync` does not flush the disk's cache (SQLite only asks for that with `PRAGMA fullfsync`),
so the 100 µs ack includes almost no real sync cost. On the VPS an `fsync` will be 0.2 to 5 ms, and the actor does one per change, so
the ceiling per room is about 1 / (the fsync time): 200 ops/s at 5 ms, 1000 at 1 ms. The disk is not known; this is arithmetic, not a measurement. It is still far above what people type (a few changes a second per person, and the
browser should compose bursts into one submit), but it is the number to measure on the target disk before relying on it.
Rebase cost was **not isolated**: the 484 of 600 rebased changes come from the correctness test, which is not timed, and the throughput test has no run at depth 0 against depth k. Throughput falls about 20% between 1 and 16 clients in run A (and not in the re-run); nothing here says how much of that is rebasing.

**Memory** (release, RSS from `ps`): 2000 empty rooms cost 12.2 MB, **about 6 KB per room** (task, channel, maps; not the text). One
room with a 10 KB text and a window of 1000 ops costs about 1.1 MB more. That is about 1 KB per op **for ops that insert ten bytes**, and
the delta also includes the store's page cache, the WAL and allocator slack, so read it as an upper bound for tiny edits. The window is
bounded by op count (default 4096), **not by bytes**, and a kept op holds its inserted text: a window of agent-style whole-region
replacements of about 240 KB each is gigabytes (a kept op holds its text as UTF-16, two bytes a unit: about 480 KB, so 4096 of them are about
2 GB, and at the 1 MiB message limit up to about 8 GiB). The real build needs
a byte bound on the window next to the count. The spike has no idle eviction, so every room ever opened stays.

**Shutdown with an open socket:** `serve_until` returned after about 100 µs, with the connection still open: the upgraded session
is a detached task and nothing tracks it (the client's socket was still open 2 s later, and the task keeps the room and the store
alive). That is the gap the `UpgradeSlot` work in `src/lib.rs` is for; I did not touch the connection loop. (`UpgradeSlot` was merged afterwards, in #116: a real WebSocket handler runs its task through `slot.spawn(|shutdown| ...)` so that the task keeps its place under the caps, is told when the server shuts down, and is aborted after the grace period. The spike's handler predates it and does not use it, so the spike still shows the gap, and the branch has not been rebased onto it.)

## What was awkward or surprising

- **The store's idempotency does not fit a server that rebases.** `Store::append` dedupes on (key, same actor, **same payload**). A
  retried submit is rebased again, over more ops, and so produces a *different* payload, and the store answers `KeyConflict`. The room
  therefore keeps its own `key → seq` index over the replay window and answers a known key before it rebases anything. Outside
  the window a retry of an applied change is refused as `too_old` and the client resyncs (the text already contains the change, so
  nothing is lost or doubled). A cleaner fix is a store mode that dedupes on the key alone, or logging the *submission* (base + original
  changes) rather than the rebased change; the second makes replay O(window) rebases per op, which is why I did not choose it.
- **`Store::append` is a blocking call** (it waits for the writer thread's reply). The actor calls it through `spawn_blocking` and
  awaits it, so the room does one commit per change and the store's group commit (`max_batch`) can never batch a single room.
  Pipelining needs a non-blocking `append` (returns a future or a ticket in queue order), or an `append_many`. Until then throughput is bound by one fsync per change.
- **No `seq` without the store.** That is what we want (the log is the order), but it means a room cannot be tested with `MemoryStore`:
  `AppState.store` is a concrete `Arc<SqliteStore>`, not `Arc<dyn Store>`.
- **An ack and an op for the same slot.** A sender must not be sent its own `op` (it would double-apply) but must see confirmation *in
  stream order*, otherwise it cannot know which received ops are before its change. So the sender gets `ack` in the place of `op`.
  After a reconnect the sender sees its own op in the replay, which must also confirm it (matched by key). Both are in the client, and
  both are easy to get wrong; the test client's strict "each durable frame is the next number" assertion found no violation, but it
  is the first thing a real client needs a test for.
- **A resync can orphan an in-flight submit (found by review, not by a test).** A resync tells the client to drop what it had not had
  confirmed, but the room never learns that a key was dropped. A client that is behind (a stuck reader that also types) can send a
  submit *before it has read the resync*: that submit reaches the room after the resync was processed, the room accepts it and sends
  only an `ack`, for a key the client has by then forgotten; the test client ignores it as "a repeated ack", so the change never reaches the client, its version lags the log by one,
  and the next `op` trips the "no gap" check. No test has a client that both overflows and has a submit in flight. Fix for the real
  build: confirm the author by a normal `op` carrying the key (as the replay path already does) and drop the special `ack`, or treat an
  `ack` or `op` with a key the client does not know as a forced resync; and add that test.
- **The room's key index ignores who sent the key.** A key still in the window is answered with the old `ack` and nothing is written,
  whatever the actor or the content: two connections that build keys the same way (`name-counter`, restarting at 1 after a reload)
  can get another author's `seq` back for a change that was never applied. The store would answer `KeyConflict` for that; the room never
  does. Real build: remember (actor, hash of the submission) with the key and answer `key_conflict` when they differ, and require a random
  per-connection part in the key.
- **Presence limits are by frames only.** An ephemeral frame is dropped when the queue is more than half full by *frame count*, but it
  still adds its bytes: enough presence frames can fill the byte budget, and the next durable frame then causes a resync. Real build:
  drop an ephemeral frame that would take the queue past half of the byte limit too.
- **Positions are UTF-16 units** because that is what CodeMirror and `String.length` use. The Rust text is `Vec<u16>`. A change that
  splits a surrogate pair would produce invalid UTF-16; the spike does not check for it (`from_utf16_lossy` hides it).
- **One change in flight per client.** The test client sends the first pending change and waits for its ack. CodeMirror's
  `sendableUpdates` returns a list; a real client should compose the pending changes (or the `submit` frame should carry a list with
  one key each), or typing speed is bounded by round trips.
- **Rebuilding a room replays the log from 1** (no snapshots) and the text is cloned on every op (`Op::apply` builds a new `Vec<u16>`,
  and the gate gets a `String` only for agents). Fine for schema-sized text (a few KB to a few hundred KB); a 1 MB text under typing would be
  O(n) per keystroke. The size limit exists partly for that.
- **The gate parses inside the room task.** An agent change is parsed with `dibibest-core` synchronously in the room's actor, so a large
  agent change stalls every editor of that room while it parses (not measured). Real build: parse on a blocking thread before the room
  takes the change, and re-check the base.
- **An agent is blocked while a person leaves the text broken.** The gate parses the *result*, so while the text does not parse, any agent
  change that does not itself repair it is refused. That may be right (an agent should not build on broken text) but it will surprise
  someone; Stage 4's suggestion mode is the natural place to hold such a change instead of refusing it.
- **The DBML parser is lenient.** My first "broken" agent change (`Table ` inserted in front of `Table t {`) parsed. The gate is only as
  strict as `dibibest-core`'s parse, and "parses" is not "means what the agent intended".
- `router_with` merges `live::routes()` in the same router, so a test cannot pass a room config through it (the routes would overlap and
  panic). I added `live::routes_with` / `routes_with_rooms`; the tests that need a small window use a bare router with the live routes plus
  one test through the real `router()`.
- The rate limiter runs in front of the WebSocket upgrade, so every (re)connect is one request against the per-IP budget. A fleet of
  tabs behind one NAT reconnecting after a deploy would hit it. Not tested.
- `axum` 0.8.9 pulls `tungstenite` 0.29, so the dev-dependency is `tokio-tungstenite = "0.29"` (0.26 would mean two copies).

## Worth keeping / not

Keep:
- The shape: room actor + durable log + bounded per-connection queue with resync. It was small (`live/room.rs` is about 800 lines including the queue, its tests and comments)
  and every failure mode I tried had a clear answer.
- `live/text.rs`: the transform is the textbook retain/delete/insert algorithm, 6 unit tests, and 1,100 random concurrent changes
  (2 × 300 inserts, 2 × 250 inserts and deletes) converge through it. Keep it, add property tests (random pairs, then random triples) and a JS-side conformance test against CodeMirror's `ChangeSet`.
- The frame set (below), with the changes noted.
- The gate as one function with a closure for the text.
- The test client: it is the executable spec of what the browser must do.

Not worth keeping as is: the stubbed auth (actor in the query string / body); lazy project creation in `Room::load`; replay from 1 with no
snapshot; the global `Rooms` map with one lock held while a room loads; `Rooms` that never evicts; `RoomHandle::submit`'s error type
(`Rejection`) which should become the protocol's error; the per-op `String` for the gate.

## Wire types worth keeping (candidate `dibibest-protocol` crate)

These compile as they are today (`serde`, `serde_json`). They have no server dependency, so they can move to a crate that the WASM
client shares.

```rust
/// One replacement: [from, to, insert]. Positions are UTF-16 units of the text the change was made on;
/// replacements are sorted by `from` and do not overlap.
pub type Change = (u32, u32, String);

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// A change made on the text as it was at `base_seq`. `key` makes a retry safe.
    /// The author is the connection's actor, never a field the client sets.
    Submit { key: String, base_seq: u64, changes: Vec<Change> },
    /// Ephemeral.
    Presence { cursor: Option<u32>, selection: Option<(u32, u32)> },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    BadFrame, BadChange, TooOld, BaseInFuture, Refused, KeyConflict, Busy, Internal,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// First frame. `text: Some` = the whole text at `seq`; `None` = "you asked to continue from `since_seq`: your text is
    /// valid there, the missed `op` frames follow, up to and including `seq`".
    Hello { seq: u64, conn: u64, actor: String, text: Option<String> },
    /// Durable. Already rebased: applies to the text at `seq - 1`. Not sent to the author of a live submit (it gets `ack`);
    /// a replayed op with one's own `key` is also a confirmation.
    Op { seq: u64, actor: String, key: String, changes: Vec<Change> },
    /// Durable, to the submitter only. Sent again unchanged for a retried key.
    Ack { key: String, seq: u64 },
    /// A submit that was not applied. `seq` is the room's number now.
    Rejected { key: Option<String>, code: RejectCode, message: String, seq: u64 },
    /// Durable marker: replace your text with this; drop what you had not had confirmed. Replaces `hello` when replay is impossible.
    Resync { seq: u64, text: String },
    /// Ephemeral.
    Presence { conn: u64, actor: String, cursor: Option<u32>, selection: Option<(u32, u32)> },
    PresenceLeft { conn: u64, actor: String },
}
```

Changes I would make before it is a crate: add `len` (length of the text the change applies to, in UTF-16 units) to `Op` so a client
need not track it (CodeMirror's `ChangeSet.of(specs, length)` needs it; see the sketch); make `Submit` carry a list of
`{key, changes}` so a client can send its pending changes in one frame; add a `version` to `Hello` for protocol changes; add
`actor_kind` (person / agent, and "on behalf of") to `Op` and `Presence`; give `Rejected` for a gate refusal a structured
`problems` list instead of one message; add a client `Resume`/`Resync` request so a client can ask for the text itself.

## What the real build still needs

- **Auth and roles.** Actor from the session or the agent grant (never a query parameter or body field); a role check in the gate
  (viewer cannot submit, commenter cannot, editor can); the WebSocket upgrade checks it before 101; revocation closes the sockets of that grant.
- **Limits.** Per project: text size (exists as a constant), clients, ops per second; per connection: frames per second (a submit and a
  presence flood are both possible), message size (1 MiB set here), pending queue (exists), and per-account connection count. The rate limiter
  in front of the upgrade counts reconnects.
- **Backpressure policy.** Spike: bounded frames and bytes, one resync per overflow. Real: count resyncs per client and close one that
  resyncs repeatedly (a client that cannot keep up is not going to); a resync carries the whole text (up to the limit), so it is itself
  a cost to bound.
- **Replay window storage.** Spike: the last N ops in memory, rebuilt by replaying the whole log. Real: snapshot every K ops with
  `save_snapshot`, load the latest snapshot and the log after it, serve replay beyond the memory window from `ops_after`, and decide whether
  the window is by count or by age (clients offline for a day).
- **Snapshots and compaction.** The store already keeps snapshots; compaction (composing old ops) is not needed for correctness, only to bound the log.
  Retention is still the owner's open question; it decides whether the op log is history (useful for version history, stage 0) or just a buffer.
- **Presence cleanup.** The spike sends `presence_left` when a connection ends. It does not time out a silent one (a half-open TCP
  connection stays "present"): needs a heartbeat/ping deadline and a presence TTL. Presence by actor kind and the person an agent acts for.
- **MCP and REST entry points.** `RoomHandle::submit` is the function they call; the spike's `agent-changes` route shows it. The agent needs
  to read the text *and its `seq`* to make a change on a version (a `get_project` tool returning both), and gets `too_old` / `refused` as tool
  errors it can act on. Agents will mostly submit whole-region replacements; a convenience that diffs "new text" against the current text on
  the server would make that easier (and the diff is then a change on the current `seq`, which cannot conflict).
- **Canvas state beside the text.** Table positions and other structure are not text. Put them in the same log as typed commands
  (`{"type":"move_table",...}`) with their own `op` kind, ordered by the same `seq`, last-writer-wins per key, validated by the gate; the text
  op and the command op share the order but not the transform. The canvas keeps the last valid schema while the text is mid-edit, as today.
  The spike's payload is `{"changes": [...]}` with `schema_version` 1 for exactly this: the payload needs a `kind`.
- **Shutdown.** Upgraded connections need tracking (`UpgradeSlot`, merged in #116: use it in the real handler): at shutdown send a close frame (a code that tells clients to
  reconnect), let queues drain for the grace period, then let the rooms finish their last append before the store is closed. In the spike
  the server returned at once and the socket lived on.
- **Room lifecycle.** Evict an idle room (no clients, no recent submits) after a delay; share the loading of a room between concurrent
  first connections without holding one lock for all projects; move the writer-thread call off `spawn_blocking` when the store offers a
  non-blocking append.
- **Observability.** Per room: seq, window, clients, rebases, resyncs, queue depth, append latency. The spike has `RoomHandle::stats` with a few of them.

## How the browser editor integrates (sketch, not built, not run)

CodeMirror 6 with `@codemirror/collab`: `collab({ startVersion, clientID })` plus a view plugin. `collab` keeps the *unconfirmed*
updates and the synced version; the protocol maps onto it like this:

```ts
// state of the client loop
const myId = `me-${crypto.randomUUID()}`     // random: it never equals an actor name, so remote updates cannot look like ours
let headKey: string | null = null            // the key of the FIRST unconfirmed update, made when it is first sent and kept until confirmed
let inflight = false                         // is a submit for headKey on the wire
let lastSeq = 0                              // the last durable frame applied (hello / op / ack / resync): they must come in order
let replayUntil = 0                          // the room's seq at connect: the replay is over when lastSeq reaches it

// `receiveUpdates` builds NEW update objects whenever a remote op arrives while a change is pending, so an update has no identity
// across calls: the key belongs to "the head of the pending list", not to an object.
function firstPending() { const [u] = sendableUpdates(view.state); return u }
function askForResync() { ws.send(JSON.stringify({ type: 'resync_request' })) }   // not in the spike's protocol: see the changes listed below

// connect
const ws = new WebSocket(`/v1/live/${project}?since_seq=${getSyncedVersion(view.state)}`)  // omit on the first connect
ws.onclose = () => { inflight = false }      // reconnect; headKey stays, so the resend repeats it

function durable(seq: number): boolean {     // the next durable frame must be lastSeq + 1; a gap means something was lost
  if (seq !== lastSeq + 1) { askForResync(); return false }
  lastSeq = seq
  return true
}

ws.onmessage = (e) => {
  const f = JSON.parse(e.data)
  switch (f.type) {
    case 'hello':   // text !== null: rebuild the editor from f.text at f.seq. text === null: keep the editor; the missed ops follow
      replayUntil = f.seq
      if (f.text !== null) rebuild(f.text, f.seq)
      pushWhenCaughtUp()
      break
    case 'op': {    // someone else's change, or our own seen again in a replay (same key)
      if (!durable(f.seq)) break
      if (f.key === headKey) confirmed()                              // our own change, replayed
      else {
        const changes = ChangeSet.of(f.changes.map(([from, to, insert]) => ({ from, to, insert })), f.len)
        view.dispatch(receiveUpdates(view.state, [{ changes, clientID: f.actor }]))
      }
      pushWhenCaughtUp()
      break
    }
    case 'ack':     // our change got number f.seq (sent again, unchanged, for a retried key)
      if (f.key !== headKey) break                                    // an ack for a key we no longer have: ignore it, the next op gap will say so
      if (!durable(f.seq)) break
      confirmed()
      pushWhenCaughtUp()
      break
    case 'resync':  // the document is replaced: pending changes are gone (see below)
      rebuild(f.text, f.seq)
      pushWhenCaughtUp()
      break
    case 'rejected':
      if (f.code === 'too_old') break                                 // a resync follows
      if (f.code === 'busy') { inflight = false; setTimeout(push, 250); break }   // nothing was written: send the same key again
      // refused / bad_change / key_conflict: the change cannot go in as it is. Show f.message, then ask for the server's text.
      askForResync()
      break
    case 'presence': case 'presence_left': /* update the remote cursors layer; ephemeral, no version */ break
  }
}

// our first pending update is the one the room has numbered: tell collab it is ours
function confirmed() {
  const u = firstPending()
  if (u) view.dispatch(receiveUpdates(view.state, [{ changes: u.changes, clientID: myId }]))
  headKey = null
  inflight = false
}

function pushWhenCaughtUp() { if (lastSeq >= replayUntil) push() }

// push: one change in flight, on the synced version
function push() {
  const u = firstPending()
  if (!u || inflight) return
  headKey ??= `${myId}-${crypto.randomUUID()}`
  inflight = true
  ws.send(JSON.stringify({ type: 'submit', key: headKey, base_seq: getSyncedVersion(view.state), changes: toTriples(u.changes) }))
}
// view plugin: update(u) { if (u.docChanged) push() }

// `@codemirror/collab` has no call that drops one unconfirmed update. A resync means rebuilding the editor state: a new
// EditorState with the synced document and a fresh `collab({startVersion})`, with `headKey` and `inflight` reset and `lastSeq = seq`.
// What the person typed and had not had confirmed is lost unless the UI keeps it: compare the old text with the new document and
// offer to re-apply the difference as a new change (a design question for the UI, not solved here).
function rebuild(text: string, seq: number) { /* view.setState(EditorState.create({ doc: text, extensions: [..., collab({ startVersion: seq, clientID: myId })] })); headKey = null; inflight = false; lastSeq = seq */ }
```

Notes: (1) `ChangeSet.of` needs the length of the document the change applies to; the spike's `op` has no `len`, which I would add. (2) CM's
`ChangeSet` has retain/delete/insert sections internally, `toTriples` is a loop over `iterChanges`. (3) `receiveUpdates` rebases the
unconfirmed updates, so the browser never needs its own transform; the Rust `transform` is for the server and for the conformance test.
(4) The tie rule must be the same in both: CM's `rebaseUpdates` maps the late change over the accepted ones with the accepted ones first,
which is what `transform(accepted, late)` does here, but I have **not** run CodeMirror against this server; the first real task is a
conformance test that feeds random concurrent changes to both implementations and compares texts. (5) Reconnect keeps the editor state
(unconfirmed updates stay in `collab`'s state), reconnects with `since_seq = getSyncedVersion`, and resubmits the pending change with the same key (`headKey`) once the
`hello` and the replay have been read (`pushWhenCaughtUp`). A resent change is built from the *current* head update, which may have been rebased
over ops received meanwhile: the key names the intent, and the room answers a key it has already applied with the old `ack` whatever the content. (6) A format/prettify assist that rewrites the whole text becomes one change per differing line
(the issue says "per-line edits"), which transforms well. (7) I could not check `receiveUpdates`, `sendableUpdates`,
`getSyncedVersion` and `rebaseUpdates` against the library source myself: the first review checked `ChangeSet.of(specs, length)` and the tie rule
against `@codemirror/state`, and a second one read the `@codemirror/collab` source and found that `receiveUpdates` builds new update objects (which
is why the sketch keys the head of the pending list, not objects). This sketch is a design, not a tested client; it also needs a `resync_request`
client frame and an `op` that carries `len`, which the spike's protocol does not have.

## Risks

- **The transform is the part nobody should trust by reading.** It is short and was exercised by tens of thousands of client-side transforms (40k to 55k per run) through the
  real room, but a divergence between the Rust and the CodeMirror tie rule would show as two browsers disagreeing. Mitigation: the conformance
  test above, and the client's `resync` on a detected mismatch (a periodic hash of the text in `hello`/`ack`, not built).
- **One fsync per change per room** until the store can be pipelined; real-disk numbers are not known (see Measurements).
- **Whole-text resync is the only recovery.** Fine at schema sizes; a project near the 1 MiB limit makes each resync expensive and each slow client a cost.
- **Agents versus a person who leaves the text broken** (see above): a product decision for Stage 4, not a technical risk.
- **Memory is per room and unbounded without eviction and without a byte bound on the window:** small for tiny edits (6 KB + about 1 KB per ten-byte op), very large for big replacements (gigabytes at a full window; see Measurements).
- **One process** is by design; a room is therefore a single point of ordering and of failure for its project. The log makes restart cheap (measured: restart test), a deploy drops the sockets, and clients must reconnect with `since_seq`.

## What the spike did not test

- Anything with a real browser, CodeMirror or JavaScript; the sketch above is unrun.
- A client that both overflows its queue (so gets a resync) and has a submit in flight (see "A resync can orphan an in-flight submit").
- A reconnect with an unconfirmed change while other clients' ops sit between it and the replay (the one reconnect-with-pending test has no other edits around it).
- An agent change that has to be rebased over concurrent human edits, with the gate seeing the rebased result (the REST-like test always uses the current `seq`).
- The tie order at room level (`a_change_made_on_an_old_version_is_rebased_by_the_room` accepts either order; only the unit test in `text.rs` pins it), and that a retried key is answered with an `ack` carrying the old `seq` (the test checks that the log did not grow).
- The cost of parsing a large agent change inside the room task.
- Linux, a real disk, or a VPS; `fsync` cost; more than ~16 concurrent clients; more than one hot room; a room under memory pressure.
- Long runs, many reconnects, half-open connections, ping/pong deadlines, TLS and a reverse proxy in front of the WebSocket (idle timeouts of the proxy).
- Authentication, roles, per-project quotas, MCP.
- A network that reorders, delays or splits frames (loopback only; TCP keeps order, so a lost frame is a lost connection).
- Snapshots, compaction and a log far longer than the window (the spike's largest was 600 ops); a restart replays from 1.
- Surrogate pairs split by a change; texts of 100 KB or more under typing; deletions in the concurrent marker test (they are in the
  convergence test, which only checks that the texts are equal, not that deletes did what the user meant).
- `since_seq` replay between `queue_frames / 2` and the window (that range is served as a resync by design, tested at the edges only).
- The room's key index against a key collision between two authors.
- The interplay with `UpgradeSlot` (#116, merged after the spike was written): the spike's handler does not use it.
- The idempotency path through the store's own dedupe (a `duplicate: true` from `append`); the room's key index answers first, and the room treats a store duplicate as an internal error.

## Recommendation

**Go, with changes.** Server-ordered text with a client-side rebase gave two concurrent writers an identical, complete text through
hundreds of interleaved changes (484 of 600 rebased by the room), survived reconnect, restart and a stuck reader without any CRDT in the
bundle, and is small enough to read in an afternoon: the author's reason for not trying a CRDT is that it would add a client dependency and, if it
is the source of truth, a second order next to the log that the gate, the agent path and the version history already need (a CRDT used
only as a relay would not; none was built or measured here). The changes: give the store a non-blocking or batched append and a
key-only idempotency mode (otherwise rebase and the store's dedupe fight, and one fsync per change caps a room), add `len` and batched
`submit` to the protocol, and prove tie-rule parity with CodeMirror's `ChangeSet` before any UI is built. Do not decide on throughput
from this spike: measure on the VPS disk first.
