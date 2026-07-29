# Inbox body prefetch: why opening mail felt slow, and what changed

Opening a message in Comail felt slow. The obvious hypothesis was that bodies are
only fetched on open — build a background prefetcher and the problem goes away.

That hypothesis was wrong, and the local database says so. **Comail already has a
background body prefetcher.** It had been running for weeks, failing on every
single inbox message of six accounts, retrying each one every sixty seconds
forever, and reporting nothing. 503 of 506 stalled messages shared one root
cause: a single IMAP fetch item the client sent unconditionally and the server
was entitled to refuse.

This document records the measurement, the protocol detail behind it, how other
clients handle the same problem, and what shipped.

---

## 1. The measured gap

The prefetcher lives in `sync/engine.rs`: `drain_missing_bodies` asks
`repo::messages::missing_bodies` for rows with `body_state = 'none'` newest-first,
and `run_backfill_pool` fetches them over a two-connection pool
(`BODY_POOL_CONNS = 2`), batched at up to 200 UIDs or 8 MB of estimated encoded
bytes per fetch. It is nudged at the end of every sync cycle. Nothing about that
design is wrong.

Its output, from the live profile before this change:

| account | messages | cached | none |
| --- | --- | --- | --- |
| `agent-services@infra-arena.ai` | 331 | 76 | **255** |
| `agents@rauta.ai` | 167 | 25 | **142** |
| `agents@infra-arena.ai` | 68 | 0 | **68** |
| `azure-anchor@infra-arena.ai` | 24 | 7 | 17 |
| `m@rauta.ai` | 104 | 88 | 16 |
| `mikko.sj.kiiskila@gmail.com` | 6825 | 6821 | 4 |
| `mikko.sampo.k@gmail.com` | 115 | 115 | 0 |

Every one of those `none` rows had a `sync_failures` row explaining itself:

| signature | rows | max attempts |
| --- | --- | --- |
| `imap error: server omitted selective content for UID n` | 503 | 8,456 |
| `mime error: invalid Base64 selective MIME section` | 3 | 8,539 |

Two things to take from that table. First, the failure is **not** absent
prefetching — it is prefetching that has failed 8,000 times. Second, **8,539
attempts at one message.** The content-stage retry used a flat 60-second delay, so
against a deterministic server refusal it was a busy loop: roughly six days of
once-a-minute connections, wake-ups and radio use, for mail that could never
arrive unchanged. The two Gmail accounts look healthy in the table only because
Gmail happens to answer the request the other servers refuse.

## 2. Root cause: an unconditional `.MIME` fetch item

Comail does not fetch whole messages during prefetch. It parses `BODYSTRUCTURE`
into a stored `mime_plan_json` and then fetches only the `text/plain` and
`text/html` sections — the right design, since it leaves attachment octets on the
server. For each planned section it asked for two items:

```
BODY.PEEK[1] BODY.PEEK[1.MIME]
```

`imap.agentmail.to` answers `BAD` to the second one. Probing it directly
confirmed the rejection is universal for that server, not message-specific — it
refuses `BODY.PEEK[1.MIME]` even on multipart messages where the section
unambiguously exists.

The server is within its rights. RFC 3501 §6.4.5 defines `<part>.MIME` as "the
[MIME-IMB] header for this part", and RFC 3501 §6.4.5 further notes that the
`.MIME` part specifier is only meaningful for parts of a multipart message. A
server that does not implement it may answer `BAD`. Because one `BAD` fails the
**entire** `UID FETCH`, one unsupported item took down a 200-message batch, and
each message in it was recorded as a per-message content failure — hence a
symptom ("server omitted selective content") that reads like a server data
problem rather than a client request problem.

The `.MIME` header was also **not needed**. It carries the part's
`Content-Type`/`Content-Transfer-Encoding`, and the v2 MIME plan already stores
both (`PlannedTextSection { mime_type, charset, transfer_encoding, size }`).
`decode_planned_text_section` reads them from the plan, so the extra fetch item
bought nothing and cost every affected account its entire inbox.

## 3. IMAP techniques, and which ones matter here

- **`BODY.PEEK[...]` vs `BODY[...]`** — `BODY[]` implicitly sets `\Seen`
  (RFC 3501 §6.4.5). Any prefetch must use `PEEK`, or the act of prefetching
  marks unread mail read. Comail's section-fetch builder emits `PEEK` for every
  item; a test now asserts no query string can contain a non-`PEEK` `BODY[`.
- **`BODYSTRUCTURE` first, then sections** — one round trip yields the part tree
  with per-part sizes and encodings, which is what lets the client decide *not*
  to download a 12 MB attachment while still getting the readable text. This is
  the mechanism that makes whole-inbox prefetch affordable at all.
- **`BODY.PEEK[TEXT]`** — the whole body after the top-level headers. Cheaper to
  implement than section planning but it pulls attachment octets on any
  multipart mail, so it is the wrong default for prefetch. It remains a
  reasonable fallback for servers with no usable `BODYSTRUCTURE`.
- **Batched `UID FETCH` over compressed UID sets** — `UID FETCH 2:4,9 (...)` in
  one command instead of N commands. Comail already batches at 200 UIDs / 8 MB.
  The cost of batching is blast radius: one `BAD` fails everything in the batch,
  which is precisely how a single unsupported item became a 500-message outage.
  Batching therefore requires per-batch capability negotiation, not just
  per-message error recording.
- **CONDSTORE / QRESYNC** (RFC 7162) — `HIGHESTMODSEQ` lets a resync ask only for
  what changed. These make *header/flag* sync cheap; they do not fetch bodies,
  so they bound how often the prefetcher is woken rather than what it fetches.
  Comail already stores `highestmodseq` per folder.
- **IDLE** (RFC 2177) — the connection that tells you new mail exists is also the
  natural trigger for filling its body, which is what "prefetch on idle" means in
  practice: fetch when the server says something arrived, not on a timer.

## 4. What other clients do

Behavior worth copying, in rough order of aggressiveness:

- **Apple Mail** offers this as an explicit account setting — *Download
  Attachments: All / Recent / None* — with message text always downloaded and
  attachments as the separately-controlled axis. That split (text always,
  attachments deferred) is the same one Comail's MIME plan implements, and it is
  the right primitive.
- **Superhuman**'s public claim is a 100 ms interaction budget, which is only
  achievable if the body is already local when the row is selected: their
  positioning ("we keep your mail on the device") implies whole-mailbox text
  caching rather than N-visible-rows.
- **Spark** and **Outlook mobile** prefetch aggressively for recent mail and
  degrade under metered connections, with attachments on-demand.
- **Gmail mobile** syncs a *window* by default (a configurable number of days of
  mail) and treats attachments as a separate toggle — a useful reminder that the
  honest knob is a recency window, not "everything forever".

The consensus is narrow and consistent: **prefetch the readable text for recent
mail eagerly; defer attachments; keep a size ceiling.** Nobody prefetches
attachment octets by default, and nobody makes the user wait for text.

## 5. When not to prefetch

- **Large messages.** A body big enough to matter is almost always attachment
  bytes. Comail already avoids these via section planning; the new full-message
  fallback (below) needed its own ceiling for the same reason.
- **`\All` / archive folders on non-Gmail servers.** Already handled:
  `missing_bodies` excludes `role = 'all'` unless the provider is Gmail, so a
  virtual all-mail folder cannot make the prefetcher re-download the account.
- **Trash.** Nothing that has lost its UID or moved folder is fetched — the
  candidate query requires `uid IS NOT NULL` and a matching `folder_id`, and the
  persist step re-checks the row's location before writing. (Observed live: one
  message moved to Trash mid-run and was correctly skipped rather than fetched
  into a stale path.)
- **A server that keeps saying no.** This is the one the old code got wrong. A
  deterministic refusal must back off, not retry every minute forever.
- Metered/cellular awareness is **not** implemented and is listed as a residual
  gap below.

## 6. What shipped

**1. Negotiate `.MIME` instead of assuming it.** `MimeHeaderMode::{Include, Omit}`
controls whether `BODY.PEEK[<section>.MIME]` is requested.
`fetch_content_sections_batch_adaptive` tries `Include`, and on an
unsupported-section `BAD` retries the same batch as `Omit`. The outcome is cached
per account for the session (`SyncCtx::section_mime_unsupported`), so a server
that refuses is asked exactly once, not once per batch. `is_unsupported_section_error`
distinguishes that `BAD` from transport errors, so a dropped connection still
triggers a reconnect rather than a silent capability downgrade.

**2. Bounded full-message rescue.** When the selective path still fails —
including the Gmail `invalid Base64` case — `rescue_bodies_with_full_fetch` fetches
`BODY.PEEK[]` for that one message and persists it, capped at
`MAX_BACKGROUND_FULL_FETCH_BYTES` (2 MB) using the `RFC822.SIZE` already stored
during header sync, so the check costs no extra round trip. Anything larger is
left to the on-open path, where the user has asked for the bytes. A transport
error during rescue aborts the chunk instead of being stamped onto every
remaining message.

**3. Exponential backoff for content retries.** `content_retry_delay_ms` doubles
from 1 minute to an 8-hour ceiling. This is what stops 8,000-attempt busy loops.

**4. Migration 020 clears the poisoned ledger.** The backoff fix alone would have
left every one of those 8,000-attempt rows sitting at the 8-hour ceiling — a user
would upgrade and still wait most of a day. 020 deletes the `sync_failures` rows
(not the messages) matching the two fixed signatures, restoring a first attempt on
the next sync cycle. If a failure is real it returns with `attempts = 1` and backs
off honestly. Scoped by signature and by `stage = 'content'`: every other failure
keeps its history, because nothing here makes it likelier to succeed.

**5. The persist guard now matches its caller.** `persist_body` takes a
`SelectivePersistGuard` rather than hardcoding `Priority`. This mattered: the
on-open path claims its row as `body_state = 'fetching'` before fetching, while
background prefetch leaves it `none`. The rescue presented `Priority`, the guard
saw `none`, and every write was silently dropped — the fetch succeeded, the log
said `cached body`, and `body_state` never moved. It also now returns whether a
row was actually written, so the caller cannot count a body it did not cache.

## 7. Verification

Run against a copy of the live profile via
`cargo run -p comail-core --example prefetch_probe`, which drives the real
`Core::sync_now` path and **never calls `get_body`** — so a `cached` result can
only have come from prefetch. The probe refuses to run against the real data dir.

Three INBOX messages on `agent-services@infra-arena.ai`, before:

```
id    body_state  uid   size   attempts  last_error
6681  none        832   15220  7291      imap error: server omitted selective content for UID 832
7260  none        1028  40299  4415      imap error: server omitted selective content for UID 1028
7439  none        1031  39448  3074      imap error: server omitted selective content for UID 1031
```

After one sync cycle:

```
=== AFTER: bodies 327/330 cached for account 4 ===
  message 7439: body_state=cached raw=.../mail/4/7439.30.1031.eml (39448 bytes)
  message 7260: body_state=cached raw=.../mail/4/7260.30.1028.eml (40299 bytes)
  message 6681: body_state=cached raw=.../mail/4/6681.30.832.eml (15220 bytes)
```

A second cycle reached **330/330**. The one row still reading `none` is a message
that had moved to Trash and lost its UID — correctly excluded from both the
progress denominator and the fetch, which is the skip rule working rather than a
miss.

Account-level: **76/331 → 330/330 cached**, from a state that had been stuck for
weeks. `cargo test -p comail-core`: 270 passing, 0 failing.

## 8. Residual gaps

- **Attachments are still on-demand, deliberately.** Only `text/plain` and
  `text/html` are prefetched. Opening a message with a large attachment still
  waits for that attachment. This matches Apple Mail's default and is the right
  trade; an Apple-Mail-style *Download attachments: recent* setting is the natural
  follow-up.
- **No metered-network awareness.** Prefetch runs the same on cellular as on
  wifi. Worth a check before this ships to anyone mobile.
- **Messages over 2 MB whose selective fetch fails** are left to the on-open path
  rather than rescued in the background. Intentional, but it means a
  large-attachment mail on a `.MIME`-refusing server is still a slow first open.
- **`session_aware` capability caching is per-session, not persisted.** Each new
  `SyncCtx` re-learns that a server refuses `.MIME` — one wasted round trip per
  account per session. Cheap to persist on `accounts` if it ever shows up in a
  trace.
- **The Gmail `invalid Base64` decode failure is worked around, not fixed.** Three
  messages hit it; the full-message rescue caches them correctly, but the
  underlying selective-decode bug is unexamined. Small blast radius, real bug.
- **No progress surface for pending bodies.** `body_progress` exists in the DB and
  the probe reads it, but the UI does not show "bodies 327/330". Sync can still
  look finished while bodies are outstanding.
