import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:isar_community/isar.dart';
import 'package:mangayomi/models/category.dart';
import 'package:mangayomi/models/chapter.dart';
import 'package:mangayomi/models/changed.dart';
import 'package:mangayomi/models/history.dart';
import 'package:mangayomi/models/manga.dart';
import 'package:mangayomi/models/settings.dart';
import 'package:mangayomi/models/source.dart';
import 'package:mangayomi/models/sync_preference.dart';
import 'package:mangayomi/models/track.dart';
import 'package:mangayomi/models/update.dart';
import 'package:mangayomi/repositories/db.dart';
import 'package:mangayomi/repositories/chapter_repository.dart';
import 'package:mangayomi/repositories/history_repository.dart';
import 'package:mangayomi/services/sync/sync_change_collector.dart';
import 'package:mangayomi/services/sync/sync_entity_applier.dart';
import 'package:mangayomi/services/sync_server.dart';
import 'package:mangayomi/services/sync_host/sync_host_providers.dart';
import 'package:mangayomi/providers/sync_providers.dart';
import 'package:mangayomi_sync/mangayomi_sync.dart';

class _ScriptedEngine extends SyncEngine {
  _ScriptedEngine(this.script) : super(MemorySyncStore());
  final Map<String, dynamic> Function(Object?) script;
  @override
  Map<String, dynamic> handle(Object? request) => script(request);
}

void main() {
  late Directory directory;
  late SyncEntityApplier applier;

  setUpAll(() async {
    await Isar.initializeIsarCore(download: true);
  });
  setUp(() async {
    directory = await Directory('build/sync-tests').create(recursive: true);
    directory = await directory.createTemp('applier-');
    isar = await Isar.open(
      [
        MangaSchema,
        ChapterSchema,
        CategorySchema,
        HistorySchema,
        TrackSchema,
        UpdateSchema,
        SettingsSchema,
        SourceSchema,
        ChangedPartSchema,
        SyncPreferenceSchema,
      ],
      directory: directory.absolute.path,
      name: 'sync_test',
    );
    applier = SyncEntityApplier()..beginSync();
  });
  tearDown(() async {
    await isar.close(deleteFromDisk: true);
    await directory.delete(recursive: true);
  });

  Map<String, dynamic> title() => {
    'clientId': 100,
    'itemType': 'MANGA',
    'name': 'Book',
    'link': '/book',
    'updatedAt': 10,
  };
  Map<String, dynamic> chapter(int timestamp, bool read, String page) => {
    'clientId': 200,
    'mangaClientId': 100,
    'name': 'Chapter',
    'url': '/c1',
    'isRead': read,
    'lastPageRead': page,
    'updatedAt': timestamp,
  };

  test(
    'a child in an earlier page waits for its parent, then is saved',
    () async {
      await applier.apply({
        'chapters': [chapter(10, false, '7')],
        'histories': [
          {
            'clientId': 300,
            'mangaClientId': 100,
            'chapterClientId': 200,
            'itemType': 'MANGA',
            'updatedAt': 10,
          },
        ],
      });
      expect(applier.requireComplete, throwsStateError);
      await applier.apply({
        'manga': [title()],
      });
      expect(applier.requireComplete, returnsNormally);
      expect(chapterRepository.getByClientId(200)!.lastPageRead, '7');
      expect(
        historyRepository.getByClientId(300)!.chapterId,
        chapterRepository.getByClientId(200)!.id,
      );
    },
  );

  test(
    'newer explicit unread and lower reread page survive stale progress',
    () async {
      await applier.apply({
        'manga': [title()],
        'chapters': [chapter(10, true, '40')],
      });
      await applier.apply({
        'chapters': [chapter(30, false, '2')],
      });
      await applier.apply({
        'chapters': [chapter(20, true, '40')],
      });
      final stored = chapterRepository.getByClientId(200)!;
      expect(stored.isRead, false);
      expect(stored.lastPageRead, '2');
      expect(stored.updatedAt, 30);
    },
  );

  test(
    'a tombstone cancels a child waiting for a now-deleted parent',
    () async {
      await applier.apply({
        'chapters': [chapter(10, false, '7')],
      });
      await applier.apply({
        'tombstones': {
          'manga': [100],
        },
      });
      expect(applier.requireComplete, returnsNormally);
      expect(chapterRepository.getByClientId(200), isNull);
    },
  );

  test(
    'history upload resolves local chapter id, not another row client id',
    () async {
      await applier.apply({
        'manga': [title()],
        'chapters': [chapter(10, false, '7')],
      });
      final local = chapterRepository.getByClientId(200)!;
      await isar.writeTxn(() async {
        await isar.settings.put(Settings());
        await isar.categorys.put(
          Category(name: 'Category', forItemType: ItemType.manga)
            ..clientId = 400
            ..updatedAt = 10,
        );
        await isar.chapters.delete(local.id!);
        local.id = 900;
        await isar.chapters.put(local);
        await isar.chapters.put(
          Chapter(mangaId: local.mangaId, name: 'Collision')
            ..id = 1
            ..clientId = 900
            ..updatedAt = 10,
        );
        await isar.historys.put(
          History(
              itemType: ItemType.manga,
              mangaId: local.mangaId,
              chapterId: local.id,
              date: '10',
            )
            ..clientId = 300
            ..updatedAt = 10,
        );
      });
      final changed = ChangedRows.collectAll(forcedUpdatedAt: 50);
      expect(changed.histories.single['chapterClientId'], 200);
      expect(changed.categories.single['updatedAt'], 50);
    },
  );

  test(
    'optional state fields do not erase an existing reading position',
    () async {
      await applier.apply({
        'manga': [title()],
        'chapters': [chapter(10, true, '9')],
      });
      await applier.apply({
        'chapters': [
          {
            'clientId': 200,
            'mangaClientId': 100,
            'url': '/c1',
            'updatedAt': 20,
          },
        ],
      });
      final stored = chapterRepository.getByClientId(200)!;
      expect(stored.isRead, true);
      expect(stored.lastPageRead, '9');
    },
  );

  Future<void> seedSync() async {
    await applier.apply({
      'manga': [title()],
      'chapters': [chapter(10, false, '7')],
    });
    await isar.writeTxn(() async {
      await isar.settings.put(Settings()..updatedAt = 10);
      await isar.syncPreferences.put(
        SyncPreference(
          syncId: 1,
          syncOn: true,
          server: SyncPreference.thisDeviceHost,
        ),
      );
    });
  }

  test('initial upload includes legacy rows with no updatedAt', () async {
    await seedSync();
    final local = chapterRepository.getByClientId(200)!;
    await isar.writeTxn(() async {
      await isar.chapters.put(local..updatedAt = null);
    });
    final container = ProviderContainer();
    addTearDown(container.dispose);
    final changed = ChangedRows.collect(
      SyncPreference(since: 0),
      container.read(synchingProvider(syncId: 1).notifier),
    );
    expect(changed.chapters.single['clientId'], 200);
    expect(changed.chapters.single['updatedAt'], 0);
  });

  test(
    'a legacy host repeating pages fails visibly without saving a cursor',
    () async {
      await seedSync();
      var calls = 0;
      final engine = _ScriptedEngine((_) {
        calls++;
        return {
          'hasMore': true,
          'cursors': {'manga': '1'},
        };
      });
      final container = ProviderContainer(
        overrides: [syncHostEngineProvider.overrideWith((_) async => engine)],
      );
      addTearDown(container.dispose);
      final sync = container.read(syncServerProvider(syncId: 1).notifier);
      expect(await sync.startSync(null, true, download: true), false);
      expect(calls, 2);
      expect(isar.syncPreferences.getSync(1)!.since, isNull);
    },
  );

  test(
    'a read while uploading is included by the next incremental sync',
    () async {
      await seedSync();
      final storeEngine = SyncEngine(MemorySyncStore());
      var edited = false;
      final engine = _ScriptedEngine((request) {
        final response = storeEngine.handle(request);
        if (!edited) {
          edited = true;
          final local = chapterRepository.getByClientId(200)!;
          final now = DateTime.now().millisecondsSinceEpoch;
          isar.writeTxnSync(
            () => isar.chapters.putSync(
              local
                ..lastPageRead = '8'
                ..updatedAt = now,
            ),
          );
          sleep(const Duration(milliseconds: 5));
        }
        return response;
      });
      final container = ProviderContainer(
        overrides: [syncHostEngineProvider.overrideWith((_) async => engine)],
      );
      addTearDown(container.dispose);
      final sync = container.read(syncServerProvider(syncId: 1).notifier);
      expect(await sync.startSync(null, true), true);
      expect(
        (storeEngine.handle({})['chapters'] as List).single['lastPageRead'],
        '7',
      );
      expect(await sync.startSync(null, true), true);
      expect(
        (storeEngine.handle({})['chapters'] as List).single['lastPageRead'],
        '8',
      );
    },
  );

  test('partial upload commits good rows but keeps retry watermarks', () async {
    await seedSync();
    await applier.apply({
      'manga': [
        {...title(), 'clientId': 500, 'name': 'Too long', 'link': 'x' * 70000},
      ],
    });
    final engine = SyncEngine(MemorySyncStore());
    final container = ProviderContainer(
      overrides: [syncHostEngineProvider.overrideWith((_) async => engine)],
    );
    addTearDown(container.dispose);
    final sync = container.read(syncServerProvider(syncId: 1).notifier);
    expect(await sync.startSync(null, true), false);
    expect(sync.unsentRows.single.name, 'Too long');
    expect((engine.handle({})['manga'] as List).single['clientId'], 100);
    expect(isar.syncPreferences.getSync(1)!.since, isNull);
    expect(isar.syncPreferences.getSync(1)!.lastSync, isNull);
    // Fixing the local entry makes the ordinary retry send it.
    final bad = isar.mangas.filter().clientIdEqualTo(500).findFirstSync()!;
    await isar.writeTxn(() async {
      await isar.mangas.put(bad..link = '/fixed');
    });
    expect(await sync.startSync(null, true), true);
    expect((engine.handle({})['manga'] as List).length, 2);
    expect(isar.syncPreferences.getSync(1)!.since, isNotNull);
  });

  test(
    'host clock offset does not hide the next local reading change',
    () async {
      await seedSync();
      final engine = SyncEngine(
        MemorySyncStore(),
        clock: () => DateTime.now().millisecondsSinceEpoch + 3600000,
      );
      final container = ProviderContainer(
        overrides: [syncHostEngineProvider.overrideWith((_) async => engine)],
      );
      addTearDown(container.dispose);
      final sync = container.read(syncServerProvider(syncId: 1).notifier);
      expect(await sync.startSync(null, true), true);
      final prefs = isar.syncPreferences.getSync(1)!;
      expect(prefs.since!, greaterThan(prefs.lastSync!));
      final editAt = prefs.lastSync! + 1;
      final local = chapterRepository.getByClientId(200)!;
      await isar.writeTxn(() async {
        await isar.chapters.put(
          local
            ..lastPageRead = '8'
            ..updatedAt = editAt,
        );
      });
      expect(await sync.startSync(null, true), true);
      expect(
        (engine.handle({})['chapters'] as List).single['lastPageRead'],
        '8',
      );
      expect(
        container.read(synchingProvider(syncId: 1)).since,
        isar.syncPreferences.getSync(1)!.since,
      );
    },
  );
}
