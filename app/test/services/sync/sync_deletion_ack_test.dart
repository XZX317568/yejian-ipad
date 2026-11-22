import 'dart:io';

import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:isar_community/isar.dart';
import 'package:mangayomi/models/category.dart';
import 'package:mangayomi/models/changed.dart';
import 'package:mangayomi/models/chapter.dart';
import 'package:mangayomi/models/history.dart';
import 'package:mangayomi/models/manga.dart';
import 'package:mangayomi/models/settings.dart';
import 'package:mangayomi/models/source.dart';
import 'package:mangayomi/models/sync_preference.dart';
import 'package:mangayomi/models/track.dart';
import 'package:mangayomi/models/update.dart';
import 'package:mangayomi/repositories/changed_part_repository.dart';
import 'package:mangayomi/repositories/db.dart';
import 'package:mangayomi/services/sync_host/sync_host_providers.dart';
import 'package:mangayomi/services/sync_server.dart';
import 'package:mangayomi_sync/mangayomi_sync.dart';

class _BeforeRequestEngine extends SyncEngine {
  _BeforeRequestEngine(this.beforeRequest) : super(MemorySyncStore());

  final void Function() beforeRequest;

  @override
  Map<String, dynamic> handle(Object? request) {
    beforeRequest();
    return super.handle(request);
  }
}

void main() {
  late Directory directory;

  setUpAll(() async {
    await Isar.initializeIsarCore(download: true);
  });

  setUp(() async {
    directory = await Directory('build/sync-tests').create(recursive: true);
    directory = await directory.createTemp('deletion-ack-');
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
      name: 'sync_deletion_ack_test',
    );
  });

  tearDown(() async {
    await isar.close(deleteFromDisk: true);
    await directory.delete(recursive: true);
  });

  test('only the captured deletion versions are acknowledged', () async {
    final repository = ChangedPartRepository();
    final sent = ChangedPart(
      actionType: ActionType.removeItem,
      isarId: 1,
      clientId: 101,
      data: '101',
      clientDate: 100,
    );
    final edited = ChangedPart(
      actionType: ActionType.removeItem,
      isarId: 2,
      clientId: 102,
      data: '102',
      clientDate: 100,
    );
    isar.writeTxnSync(() {
      isar.changedParts.putSync(sent);
      isar.changedParts.putSync(edited);
    });
    final versions = [
      ChangedPartVersion.of(sent),
      ChangedPartVersion.of(edited),
    ];

    final added = ChangedPart(
      actionType: ActionType.removeItem,
      isarId: 3,
      clientId: 103,
      data: '103',
      clientDate: 300,
    );
    isar.writeTxnSync(() {
      // The same row changed after capture: a new version, not the one sent.
      edited
        ..clientId = 202
        ..data = '202'
        ..clientDate = 200;
      isar.changedParts.putSync(edited);
      isar.changedParts.putSync(added);
    });

    await repository.clearVersions(versions, true);

    expect(isar.changedParts.getSync(sent.id!), isNull);
    expect(isar.changedParts.getSync(edited.id!)?.clientId, 202);
    expect(isar.changedParts.getSync(added.id!)?.clientId, 103);
  });

  test('a deletion made during upload stays out of the ack', () async {
    isar.writeTxnSync(() {
      isar.settings.putSync(Settings()..updatedAt = 10);
      isar.syncPreferences.putSync(
        SyncPreference(
          syncId: 1,
          syncOn: true,
          server: SyncPreference.thisDeviceHost,
        ),
      );
      isar.changedParts.putSync(
        ChangedPart(
          actionType: ActionType.removeItem,
          isarId: 101,
          clientId: 101,
          data: '101',
          clientDate: 1,
        ),
      );
      isar.changedParts.putSync(
        ChangedPart(
          actionType: ActionType.removeItem,
          isarId: 102,
          clientId: 102,
          data: '102',
          clientDate: 1,
        ),
      );
    });

    var added = false;
    final engine = _BeforeRequestEngine(() {
      if (added) return;
      added = true;
      isar.writeTxnSync(() {
        isar.changedParts.putSync(
          ChangedPart(
            actionType: ActionType.removeItem,
            isarId: 103,
            clientId: 103,
            data: '103',
            clientDate: DateTime.now().millisecondsSinceEpoch,
          ),
        );
      });
    });
    final container = ProviderContainer(
      overrides: [syncHostEngineProvider.overrideWith((_) async => engine)],
    );
    addTearDown(container.dispose);
    final sync = container.read(syncServerProvider(syncId: 1).notifier);

    expect(await sync.startSync(null, true), true);

    final left = isar.changedParts
        .filter()
        .actionTypeEqualTo(ActionType.removeItem)
        .findAllSync();
    expect(left.map((row) => row.clientId), [103]);
    final tombstones =
        (engine.handle({})['tombstones'] as Map)['manga'] as List;
    expect(tombstones, containsAll([101, 102]));
    expect(tombstones, isNot(contains(103)));

    // The next ordinary sync sends and acknowledges the new deletion.
    expect(await sync.startSync(null, true), true);
    expect(
      isar.changedParts
          .filter()
          .actionTypeEqualTo(ActionType.removeItem)
          .findAllSync(),
      isEmpty,
    );
  });
}
