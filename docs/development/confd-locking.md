Title: Locks in couch-confd
Description: The order the daemon's locks are taken in, what may not happen while one is held, and the check that holds a debug build to it.
Order: 8

# Locks in couch-confd

Read this before you add a lock, a cache, a background thread or a callback to
`daemon/couch-confd`. It is for people changing the daemon, not for people
writing an integration.

The daemon has four HTTP workers per port, a thread per panel request on
`plugin.sock` (at most eight), a one-second tick, the legacy converter, the
ten-second package sweep and one worker per running package child. They share
the locks below. A pair of them taken in opposite orders by two threads stops
the daemon for good, and because almost every request reads the configuration,
"stops" means the whole API. That has already happened once in review
(couch #256: the sweep held the package registry and asked for the
configuration; deleting a connection held the configuration and asked for the
registry).

## The order

A thread may **wait for** a lock only while every lock it already holds is
higher up this list. Two locks on the same line are never waited for together.

| # | Lock | Where | What it protects | Held for |
|---|------|-------|------------------|----------|
| 1 | connection gate (read/write) | `gate_for`, `api/connections.rs` | that a connection's folder exists for as long as a request is working in it | a whole `/api/connections/<id>/…` request (read); a deletion (write) |
| 2 | pairing session | `PairingSlot::session`, `plugins.rs` | one pairing conversation and its package child | one step, up to 12 s inside the package |
| 3 | connection settings lock | `lock_for`, `api/connections.rs` | one connection's settings, key and running child: one request at a time per connection | a whole request to the package or device, up to 12 s |
| 4 | configuration store | `Api::store`, `api.rs` | `config.json` and its revision | one read, or one edit and its write to disk |
| 5 | catalog generations | `Runtime::catalog_generations` | which manifests the tick has already copied into the configuration | the tick's pass |
| 6 | package registry | `Runtime::endpoints` | which package child serves which connection | a map operation |
| 7 | children listing cache | `Runtime::children` | the last listing of each connection | a map operation |
| 8 | pairing map | `Runtime::pairings` | which session each connection is pairing through | a map operation |
| 9 | pairing times | `Runtime::paired_at` | when each connection last finished pairing | a map operation |
| 10 | unreadable key notes | `Runtime::unreadable_keys` | what has already been said in the log | a set operation |

The names and numbers in code are `lock_order::level` (`ConnectionGate = 10`
… `UnreadableKeys = 100`, with gaps). A new lock gets a line here and a level
there, between the two locks it sits between.

`try_lock`, `try_read` and `try_write` are outside the rule, because a thread
that does not wait cannot be half of a deadlock. The daemon depends on that in
three places, and they must stay tries:

- Locks 2 and 3 are held across a package's answer, so **every request path
  only ever tries them** (or tries for a bounded time with `patiently`) and
  answers `busy`. Nothing may wait for one while holding anything.
  A request to a package, and a listing, try lock 3 for a quarter of a second
  (`REQUEST_LOCK_WAIT`): a read takes about thirty milliseconds, and without
  that the panel and a phone reading one bridge refused each other one time in
  ten. It stays well under the 750 ms a request may queue for, and four
  requests stuck behind a stalled connection free the four workers again in
  that quarter of a second.
- The gate is written with `try_write` only. `std`'s `RwLock` makes new readers
  queue behind a waiting writer, so a deletion that *waited* behind one slow
  request would stall every other request to that connection with it.
- Deleting a connection tries all of a connection's settings locks while it
  holds the gate; the sweep tries each session while it holds the pairing map.

## Not in the list, and why

These are not checked by `lock_order`. The rule for all of them is the same:
take nothing else while holding one.

- **The package store's file lock** (`integrations/.lock`, shared for readers
  and leases, exclusive for install, update, rollback, removal and giving a
  package its user) and the package manager's own (`management/.lock`). Every
  wait on them is bounded (250 ms for a key press, 3 s for a lease or a
  mutation) and ends in `busy`, so they cannot deadlock; they can stall. They
  sit between 5 and 6: the store takes a lease while it writes
  (`Store::write`), a conversion takes one while it holds the store, and
  nothing that holds one goes back for the configuration, while nothing that
  holds any of 6 to 10 asks the package store for anything but a state file.
  Each acquisition opens the file again, so **a thread that holds a lease and
  then needs the exclusive lock waits on itself** until it times out: ask for
  a package's user (`policy`) before taking a lease.
- Leaves: the gate and settings-lock tables inside `gate_for`/`lock_for`,
  `Auth::state`, the `local_name` log, `LegacyConversion::state`, the built-in
  TV pairing tables (`sessions()` in `streaming_tv.rs` and `airplay.rs`), the
  open Matter controllers, the package manager's `operation`/`pending`/`cache`
  and its busy flag, and `PENDING_IDENTITIES` in `couch-integrations`.
- An `Endpoint`'s queue (`clients/couch-plugin/src/host.rs`) is a channel, not
  a lock: sending never waits (`busy` when eight are queued), and the caller
  then waits for the answer while holding locks 1 and 3 and nothing else.

## Never call out while holding 4 to 10

Locks 4 to 10 are needed in passing by almost every request. While a thread
holds one it must not:

- ask a package child anything, or start or stop one on purpose;
- call a closure or a trait object it was handed (that is how the sweep came to
  ask for the configuration: `reap` was given a function that read it);
- touch the network, run `apk`, or wait for another thread;
- wait on anything longer than a map operation. (The configuration store is
  the one exception to this last line: it writes and syncs `config.json` and
  takes a package-store lease while it is held. That is its job.)

`lock_order::calling_out("what")` marks the start of something slow. Put it in
front of any new one - a media fetch, a watch that waits for a package - and a
debug build panics if a thread gets there holding any of 4 to 10.

Three places do not live up to this today, all bounded and all known:

1. Deleting a connection stops its package child, and tells a pairing in
   flight that it is over (up to 2 s), while it holds the configuration store.
2. Dropping a `Running` entry kills its child and joins its worker. `retain`,
   `clear` and `remove` on the registry do that under lock 6. It never waits
   for a request (whoever is mid-request holds its own reference, so the last
   one is dropped by them, outside the registry), only for a killed process
   to be reaped. `retire_endpoint` shows the better shape.
3. Lock 5 is held for the tick's whole pass, package-store reads included. It
   is the tick thread's alone; nothing else may ever take it.

## The patterns

**Snapshot, then release.** Read what you need out of the configuration into a
value, let go, then do the work. `Api::with` closures return data, never act.
The sweep is the example: `connections_in_use()` builds the set first, and
`reap` is handed something that only looks things up in it.

**Take things out, then drop them.** Remove from a map under its lock, let the
lock go, then drop or end what you took out: `retire_endpoint`, `end_pairing`,
`pair_start` replacing a session.

**Try, and say busy.** Anything held across a package's answer is tried, never
waited for, and the caller gets `busy` (HTTP 503 or 409, `Error::Busy` on the
panel's socket). When the thing in hand cannot be asked for again - a key a
device has just issued - try for a bounded time with `patiently` and park the
result if that runs out (`write_done`).

**Mark, do not wait.** To end something another thread is in the middle of,
take it out of its map and set a flag it checks when it comes back
(`PairingSlot::cancelled`). Never wait for the thread.

**A long wait holds nothing and no worker.** There are four workers. A request
that waits for something to happen (a watch, a long-poll) must not hold the
gate, a settings lock or any of 4 to 10 while it waits, and must not be able to
occupy all four workers: with the gate held, a connection being watched could
never be deleted, and four open watches would be the whole API.

## The check

`daemon/couch-confd/src/lock_order.rs`. Locks 1 to 10 are `RankedMutex<Level, T>`
or `RankedRwLock<Level, T>`, with the same methods and results as the `std`
types. In a debug build each thread keeps a list of what it holds, and:

- waiting for a lock that is not below everything held panics with both names,
  on the first run of that code path, on one thread, with nothing to time;
- `calling_out` panics under any of 4 to 10;
- tries are recorded but never refused.

`cargo test` is a debug build, so every daemon test runs under it, and so does
a host daemon started from `target/debug`. A release build compiles it away:
`lock` is the inner `Mutex::lock` and returns the `std` guard.

What it cannot see: the file locks and leaves above, anything in another
process, a `Drop` that does slow work, and any path no test or debug run goes
down. A new lock that is left as a plain `Mutex` is invisible to it, which is
why a new lock gets a level.
