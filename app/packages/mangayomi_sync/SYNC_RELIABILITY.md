# Sync reliability handoff — 2026-11-17

Baseline: Mangayomi v0.9.96, parent repository archive `39381ba`.
This change is limited to the shared sync package, the client's sync services
and outbox, and sync tests.

## Implemented behavior

- Adopted the relevant service/package changes from upstream PR #1168,
  merge commit `51ade6e726ee21f14f1776cfab8221bb62fdf594`. Link, chapter URL
  and optional address limits are 65536 UTF-16 code units. Both
  `lib/services/sync_host/sync_host_service.dart` and
  `server/lib/src/sync_server.dart` call this same package's `SyncEngine`;
  neither has a separate schema to patch. The app and standalone server
  must each be rebuilt to use it.
- The client fits display text without cutting surrogate pairs and omits
  unusable optional addresses in the wire copy. Identifiers and reading
  state are never shortened. Invalid rows and dependent children are
  named and withheld. Unlike the upstream fit, excess category memberships
  are rejected rather than silently truncated.
- Valid rows still upload when another row is invalid. Partial sync returns
  false, records details in the existing error report, and manual sync uses
  the existing error toast with its details entry. Both watermarks stay
  unchanged, so fixing a bad row and invoking ordinary sync retries it.
  `SyncServer.unsentRows` exposes the last attempt's diagnostics. No
  localization files or new UI were added; diagnostic strings are English.
- Upload batches finish parents before children. Large deletion lists are
  split too, after upserts. UTF-8 byte limits keep upload chunks at about
  8 MiB, below the host's 25 MiB ceiling. A single oversized row/settings
  object is reported and retained for retry.
- Download cursors retain each entity's position after its last page;
  differently sized entities no longer restart one another's pagination.
  Children arriving before their parent in another page are deferred and
  retried. Unresolved children prevent a successful watermark. Tombstones
  remove matching deferred records. A repeating cursor from an older host
  ends with a visible error instead of an endless loop.
- Upload selection uses the previous local sync-start time (`lastSync`),
  while download selection uses the host cursor (`since`). Edits made
  during a sync remain eligible next time. An observed local clock rollback
  triggers a full upload and no longer stalls auto-sync scheduling.
  Initial upload includes legacy rows whose `updatedAt` is null.
- History export resolves `chapterId` as a local ID, avoiding a collision
  with another chapter's sync ID. Upload-only stamps categories as well
  as the other entities.
- Received chapter state preserves omitted optional progress fields. A
  newer explicit `isRead: false`, bookmark removal, and a lower current
  page are applied as given. There is no max-page or boolean-OR merge.
- Concurrent calls to the same sync notifier are rejected while it is busy.
  HTTP, peer, and local transport refusals retain status and field details.
- Deletion acknowledgements retain the exact pending-change versions captured
  before the upload. The ack re-reads each row and deletes it only while its
  id, action, item id, client id, data and client-date are unchanged, all in
  one write transaction. A deletion or change made while the request is in
  flight therefore stays queued for the next sync instead of being cleared
  without being sent.
- Clearing the entire outbox after a database restore is intentionally
  different: the restored database has replaced the state those rows
  described, so that path is not an upload acknowledgement.

## Verification and commands

Run from `D:\Documents\aidoku\app`, using the project Flutter 3.50/Dart 3.14.
Use one Flutter process at a time; all commands below avoid dependency resolution.

```powershell
& ..\tools\flutter.ps1 test --no-pub --concurrency=1 `
  test/services/sync/ `
  test/services/sync_host/isar_sync_store_test.dart `
  test/services/sync_oauth_callback_test.dart

& ..\tools\flutter.ps1 analyze --no-pub `
  lib/services/sync lib/services/sync_server.dart `
  packages/mangayomi_sync/lib test/services/sync

& ..\tools\flutter.ps1 test --no-pub --concurrency=1 `
  test/core/architecture/architecture_test.dart
```

Evidence logs are under `app/build/sync-tests/`. Shared MemorySyncStore
tests and the Isar host-store suite passed, including the new long-link,
mixed-pagination and reread regressions. Real Isar client tests also cover
partial-upload retry, missing parents, ID collision, host-clock offset,
in-flight reading edits, legacy null timestamps, old-host page loops, and
deletion acknowledgement racing with a new deletion.
Expected error logs inside negative tests are not failed assertions.

The standalone server's SQLite/native transport suite was not run:
the app's resolved dependencies do not include the standalone server's
`sqlite3`/`test` environment. No dependency files were changed to work
around this. Once that environment is prepared by the build owner, run:

```powershell
# In app/packages/mangayomi_sync, using the project Dart:
dart test
# In app/server, after its own dependencies are available:
dart test test/database_test.dart
# Native host tests additionally need the server native library:
# app/server/native: cargo build
# app/server: dart test test/sync_server_test.dart
```

No device, real paired host, real service, or private library was used.
TLS and actual iPad/Android operation remain device acceptance work.

## Known gaps and recommended next owners

1. **Conflicting explicit operations still use device-clock whole-row LWW.**
   `SyncEngine._upsert` compares `updatedAt` and clamps only dates more than
   one day ahead. A clock five minutes fast can defeat a later unread/reset
   on another device; equal timestamps are first-writer-wins. Clamping also
   does not rewrite the uploading client's local timestamp. The upload
   selection fix prevents omission, but does not solve conflict ordering.
   Timestamp-only metadata edits can also replace unrelated progress.

   Recommended protocol work: stable operation IDs plus per-device
   monotonic counters and a host revision/acknowledgement; carry the base
   revision for progress, read/unread, bookmarks and deletion separately.
   Define deliberate reset/reread as an operation, not max progress.
   Persist pending operations transactionally with local edits, acknowledge
   exact versions, negotiate capability and migrate both clients/hosts.
   Test skew in both directions, equal timestamps, reconnect, retry,
   backup restore and stale deletion before claiming explicit-operation safety.

2. **Rereading an already-read chapter is not saved by the reader.**
   `lib/modules/manga/reader/providers/reader_controller_provider.dart`
   returns early for `chapter.isRead` at the entry and transaction recheck
   of `updateLastReadPage`. The current reader also stores `'1'` on completion.
   Reader owner should separate current reading position from completion,
   preserve intentional reread progress, and test close/reopen and sync.
   The sync tests prove preservation of an explicit newer unread/lower-page
   record once produced, not that the existing reader produces it.

3. **Partial sync deliberately replays the previous successful interval.**
   This preserves retry data but can retransmit valid rows until the invalid
   item is fixed. A persistent quarantine/outbox with exact acknowledgements
   would allow the clean subset's watermark to advance independently.
   The existing diagnostics page is the current place to inspect skipped rows;
   a dedicated localized per-item repair/retry screen is UI-owner work.

4. **Older/legacy servers keep their own limits and behavior.**
   A refusal now names the row and field, but this change cannot update an
   external host automatically. Unsent local records stay available.
   Settings and malformed deletion envelopes remain request-level validation
   failures; no arbitrary real user operation is silently discarded.
