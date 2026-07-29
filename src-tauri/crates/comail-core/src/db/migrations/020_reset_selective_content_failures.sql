-- Retire the retry ledger for content failures that were caused by a client bug,
-- not by the server or the message.
--
-- Background prefetch asked for `BODY.PEEK[<section>.MIME]` unconditionally.
-- Some IMAP servers (AgentMail's, and it is legal to do so -- RFC 3501 only
-- requires `.MIME` on multipart parts, and a server may reject a section it does
-- not implement) answered `BAD`, which failed the whole batch and surfaced
-- per-message as "server omitted selective content for UID n". The same rows
-- then retried on a flat 60s schedule against a refusal that could never change:
-- this database reached 8,539 attempts on a single message, i.e. days of
-- once-a-minute radio wake-ups for mail that was never going to arrive.
--
-- The client now negotiates `.MIME` and falls back to a full RFC822 fetch, so
-- these rows are retryable again. But `attempts` also drives the new exponential
-- backoff, and at four-figure attempt counts every one of them would sit at the
-- eight-hour ceiling -- the user would upgrade and still wait most of a day for
-- their inbox. Deleting the ledger row (not the message) restores a first
-- attempt on the next sync cycle; if the failure is real it comes straight back
-- with attempts=1 and backs off honestly from there.
--
-- Scoped to the two signatures this fix addresses. Any other content failure
-- keeps its history, because nothing here makes it more likely to succeed.
DELETE FROM sync_failures
WHERE stage = 'content'
  AND (
    last_error LIKE '%omitted selective content%'
    OR last_error LIKE '%invalid Base64 selective MIME section%'
  );
