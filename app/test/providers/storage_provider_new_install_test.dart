import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:isar_community/isar.dart';
import 'package:mangayomi/models/settings.dart';
import 'package:mangayomi/models/source.dart';
import 'package:mangayomi/providers/storage_provider.dart';

void main() {
  late Directory directory;
  late Isar db;

  setUpAll(() async {
    TestWidgetsFlutterBinding.ensureInitialized();
    final overrides = HttpOverrides.current;
    HttpOverrides.global = null;
    try {
      await Isar.initializeIsarCore(download: true);
    } finally {
      HttpOverrides.global = overrides;
    }
  });

  setUp(() async {
    directory = Directory.systemTemp.createTempSync('yejian_settings');
    db = await Isar.open(
      [SettingsSchema, SourceSchema],
      directory: directory.path,
      name: 'yejian_new_install_${directory.path.hashCode}',
    );
  });

  tearDown(() async {
    await db.close(deleteFromDisk: true);
    if (directory.existsSync()) directory.deleteSync(recursive: true);
  });

  test('seeds only when there is no Settings row', () async {
    expect(
      await seedNewInstallSettingsIfMissing(db, isTvDevice: false),
      isTrue,
    );
    final seeded = (await db.settings.get(227))!;
    expect(seeded.useFloatingNavigationBar, isTrue);
    expect(seeded.followSystemTheme, isTrue);
    expect(seeded.enableDiscordRpc, isFalse);
    expect(seeded.checkForAppUpdates, isFalse);
    expect(seeded.displayType, DisplayType.shelf);
    expect(seeded.hideItems, contains('/AnimeLibrary'));
  });

  test('never rewrites a preference that already exists', () async {
    await db.writeTxn(
      () async => db.settings.put(
        Settings()
          ..useFloatingNavigationBar = false
          ..followSystemTheme = false
          ..enableDiscordRpc = true
          ..checkForAppUpdates = true
          ..displayType = DisplayType.list
          ..hideItems = const ['/trackerLibrary'],
      ),
    );

    expect(
      await seedNewInstallSettingsIfMissing(db, isTvDevice: false),
      isFalse,
    );

    final existing = (await db.settings.get(227))!;
    expect(existing.useFloatingNavigationBar, isFalse);
    expect(existing.followSystemTheme, isFalse);
    expect(existing.enableDiscordRpc, isTrue);
    expect(existing.checkForAppUpdates, isTrue);
    expect(existing.displayType, DisplayType.list);
    expect(existing.hideItems, ['/trackerLibrary']);
  });
}
