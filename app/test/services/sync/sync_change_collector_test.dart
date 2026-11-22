import 'package:flutter_test/flutter_test.dart';

import 'dart:convert';

import 'package:mangayomi/models/sync_preference.dart';
import 'package:mangayomi/services/sync/sync_change_collector.dart';
import 'package:mangayomi_sync/mangayomi_sync.dart';

void main() {
  Map<String, dynamic> manga(int clientId, String name, String link) => {
    'clientId': clientId,
    'source': 'Demo',
    'link': link,
    'itemType': 'MANGA',
    'name': name,
    'description': 'd' * 12000,
    'updatedAt': 10,
  };

  ChangedRows rows(List<Map<String, dynamic>> manga) => ChangedRows.fromRows(
    categories: const [],
    manga: manga,
    chapters: const [],
    tracks: const [],
    histories: const [],
    updates: const [],
    deleted: null,
    settings: null,
    deletionVersionsToAck: const [],
  );

  test('a row too long to send is left out, and the rest still goes', () {
    final changed = rows([
      manga(1, 'Fits', '/fits'),
      manga(2, 'Too long', 'x' * 70000),
    ]);

    expect([for (final m in changed.manga) m['clientId']], [1]);
    expect(changed.unsent.single.toString(), '"Too long" (link)');

    // What is sent passes the other end's checks: it is not refused whole.
    for (final chunk in changed.chunked(5000)) {
      expect(
        () => validateSyncRequest({'since': 0, ...chunk}),
        returnsNormally,
      );
    }
  });

  test('a long description is shortened, not a reason to leave out', () {
    final changed = rows([manga(1, 'Fits', '/fits')]);
    expect(changed.unsent, isEmpty);
    expect((changed.manga.single['description'] as String).length, 10000);
  });

  test('a malformed state is isolated with its dependent children', () {
    final changed = ChangedRows.fromRows(
      categories: [],
      manga: [
        manga(1, 'Invalid', '/one')..['itemType'] = 'BAD',
        manga(2, 'Good', '/two'),
      ],
      chapters: [
        {'clientId': 11, 'mangaClientId': 1, 'updatedAt': 10},
        {'clientId': 12, 'mangaClientId': 2, 'updatedAt': 10},
      ],
      histories: [
        {
          'clientId': 21,
          'mangaClientId': 1,
          'chapterClientId': 11,
          'itemType': 'MANGA',
          'updatedAt': 10,
        },
      ],
      tracks: [],
      updates: [],
      deleted: null,
      settings: null,
      deletionVersionsToAck: [],
    );
    expect(changed.unsent.map((e) => e.field), [
      'itemType',
      'mangaClientId',
      'mangaClientId',
    ]);
    final engine = SyncEngine(MemorySyncStore());
    for (final chunk in changed.chunked(2)) {
      engine.handle(chunk);
    }
    expect((engine.handle({})['manga'] as List).single['clientId'], 2);
    expect((engine.handle({})['chapters'] as List).single['clientId'], 12);
  });

  test('every parent is sent before a child in a later manga batch', () {
    final changed = ChangedRows.fromRows(
      categories: [],
      manga: [for (var i = 1; i <= 3; i++) manga(i, 'M$i', '/$i')],
      chapters: [
        {'clientId': 11, 'mangaClientId': 3, 'updatedAt': 10},
      ],
      histories: [
        {
          'clientId': 21,
          'mangaClientId': 3,
          'chapterClientId': 11,
          'itemType': 'MANGA',
          'updatedAt': 10,
        },
      ],
      tracks: [],
      updates: [],
      deleted: null,
      settings: null,
      deletionVersionsToAck: [],
    );
    final engine = SyncEngine(MemorySyncStore());
    for (final chunk in changed.chunked(2)) {
      engine.handle(chunk);
    }
    final result = engine.handle({});
    expect((result['manga'] as List).length, 3);
    expect((result['chapters'] as List).single['clientId'], 11);
    expect((result['histories'] as List).single['clientId'], 21);
  });

  test('long unicode rows are batched by encoded bytes as well as count', () {
    final changed = rows([
      for (var i = 1; i <= 4; i++)
        manga(i, 'M$i', '/$i')..['description'] = '漫' * 1000,
    ]);
    final chunks = changed.chunked(5000, maxBytes: 7000);
    expect(chunks.length, 2);
    for (final chunk in chunks) {
      expect(utf8.encode(jsonEncode(chunk)).length, lessThan(7000));
    }
  });

  test('bulk deletion is also split at the server row limit', () {
    final changed = ChangedRows.fromRows(
      categories: [],
      manga: [],
      chapters: [],
      histories: [],
      tracks: [],
      updates: [],
      settings: null,
      deletionVersionsToAck: [],
      deleted: {
        'manga': [1, 2, 3, 4, 5],
      },
    );
    final chunks = changed.chunked(2);
    expect(chunks.length, 3);
    for (final chunk in chunks) {
      expect(
        ((chunk['deleted'] as Map)['manga'] as List).length,
        lessThanOrEqualTo(2),
      );
      validateSyncRequest(chunk);
    }
  });

  test('an oversized parent also leaves its children queued for retry', () {
    final changed = ChangedRows.fromRows(
      categories: [],
      manga: [manga(1, 'Too big', '/one')],
      chapters: [
        {'clientId': 11, 'mangaClientId': 1, 'updatedAt': 10},
      ],
      histories: [],
      tracks: [],
      updates: [],
      settings: null,
      deleted: null,
      deletionVersionsToAck: [],
    );
    expect(changed.chunked(5000, maxBytes: 1024), [{}]);
    expect(changed.unsent.map((e) => e.field), [
      'request size',
      'mangaClientId',
    ]);
  });

  test('upload cutoff uses the device clock, never the host cursor', () {
    final prefs = SyncPreference(since: 900000, lastSync: 100);
    expect(localUploadSince(prefs, now: 200), 100);
    // A backwards correction must not hide edits behind the old time.
    expect(localUploadSince(prefs, now: 50), 0);
    prefs.since = 0;
    expect(localUploadSince(prefs, now: 200), 0);
  });
}
