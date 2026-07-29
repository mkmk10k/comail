# rauta: patch index

| Commit / id | Summary | Risk (update path? profile storage?) |
|-------------|---------|--------------------------------------|
| `00073c9` | Bundle: hover-target + U archive/unsub + AgentMail Trash + UPDATE_CHANNEL lock + MANIFEST/check-customs/VERIFICATION | **Touches update path** (lock only). TCs L1: UPD-00, CUSTOM-01 partial. Excludes: signed Update, BOOT-CHANNEL |
| hover-target-triage | Pointer hover beats cursor for triage targets; multi-select still wins | No update path |
| u-archive-unsubscribe | `U` = archive + List-Unsubscribe when present; `Shift+U` = read toggle | No update path |
| **one-click-unsubscribe** | RFC 8058 One-Click POST in Rust (`unsubscribe_message`); honest toasts; migration `019_list_unsubscribe_post`; `header_raw` fix for List-Unsubscribe Address parse | No update path |
| **inbox-body-prefetch** | Negotiate `BODY.PEEK[<n>.MIME]` instead of assuming it; bounded full-MIME rescue; exponential content-retry backoff; migration `020_reset_selective_content_failures` | No update path. **Migration deletes `sync_failures` rows** (retry ledger only, never messages) |
| agentmail-archive-to-trash | Archive falls through to Trash when no Archive/All Mail; Shift+E restores | No update path |
| update-channel-lock | `UPDATE_CHANNEL=locked` refuses check/install until personal channel | **Touches update path** |

FaceTime/WhatsApp presets: already merged on `master` (prior PRs).

### One-click design notes (defensible)

- **RFC 2369** carries `List-Unsubscribe` HTTPS and/or mailto URIs (angle brackets preferred; some ESPs send bare HTTPS).
- **RFC 8058** adds `List-Unsubscribe-Post: List-Unsubscribe=One-Click`. Clients MUST POST `List-Unsubscribe=One-Click` as `application/x-www-form-urlencoded`, HTTPS only, no cookies/credentials; senders MUST NOT redirect the POST. Success = HTTP 2xx.
- **Gmail/Yahoo (2024+) / Microsoft (2025+)** require One-Click for bulk marketing/subscribed mail. DKIM must cover both headers (sender-side).
- Comail previously opened the URL with GET in a browser and claimed success — wrong verb/layer. Mailto opened a composer and never sent.

### Inbox body prefetch design notes (defensible)

- Comail already had a background body prefetcher (`drain_missing_bodies` →
  `run_backfill_pool`). It was not missing; it was failing on every inbox message
  of six accounts, 503 rows with up to **8,456 retry attempts each**.
- Root cause: prefetch asked for `BODY.PEEK[<section>.MIME]` unconditionally.
  **RFC 3501 §6.4.5** defines `.MIME` only for parts of a multipart message, and a
  server may answer `BAD` — `imap.agentmail.to` does, universally. One `BAD` fails
  the whole `UID FETCH`, so one unsupported item took out a 200-message batch.
- The header was redundant: the v2 `mime_plan_json` already stores
  `mime_type` / `charset` / `transfer_encoding` / `size` per section.
- `BODY[...]` implicitly sets `\Seen` (RFC 3501 §6.4.5) — every prefetch item is
  `PEEK`, asserted by test.
- The flat 60s content retry was a busy loop against a deterministic refusal
  (~6 days of once-a-minute wake-ups per message). Now doubles 1 min → 8 h.
- Backoff alone would have parked every poisoned row at the 8-hour ceiling, so
  migration 020 clears the ledger for exactly the two fixed signatures.
- Full research + measurements: `docs/research/inbox-body-prefetch.md`.