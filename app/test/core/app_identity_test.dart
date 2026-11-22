import 'package:flutter_test/flutter_test.dart';
import 'package:mangayomi/core/app_identity.dart';
import 'package:mangayomi/models/settings.dart';

void main() {
  test('has an independent visible and install identity', () {
    expect(AppIdentity.productName, '页间');
    expect(AppIdentity.englishName, 'Yejian');
    expect(AppIdentity.applicationId, 'app.yejian.reader');
    expect(AppIdentity.applicationId, isNot(startsWith('com.kodjodevf')));
    expect(AppIdentity.storageFolderName, isNot('Mangayomi'));
    expect(AppIdentity.linkScheme, 'yejian');
    expect(AppIdentity.legacyLinkScheme, 'mangayomi');
  });

  test('a brand-new install starts as a manga reader', () {
    final settings = newInstallSettings(isTvDevice: false);
    expect(settings.hideItems, contains('/AnimeLibrary'));
    expect(settings.hideItems, contains('/NovelLibrary'));
    expect(settings.hideItems, isNot(contains('/MangaLibrary')));
    // The full paper shelf, not the "Continue" carousel.
    expect(settings.displayType, DisplayType.shelf);
  });

  test('older display modes keep the integers already on disk', () {
    expect(DisplayType.compactGrid.index, 0);
    expect(DisplayType.comfortableGrid.index, 1);
    expect(DisplayType.coverOnlyGrid.index, 2);
    expect(DisplayType.list.index, 3);
    expect(DisplayType.shelf.index, 4);
  });

  test('the shelf survives the settings JSON used by backup and sync', () {
    final settings = newInstallSettings(isTvDevice: false);
    final json = settings.toJson();
    expect(json['displayType'], 4);
    expect(Settings.fromJson(json).displayType, DisplayType.shelf);
  });

  test('new-install defaults follow the system and stay quiet', () {
    final settings = newInstallSettings(isTvDevice: false);
    expect(settings.followSystemTheme, isTrue);
    expect(settings.useFloatingNavigationBar, isTrue);
    expect(settings.enableDiscordRpc, isFalse);
    expect(settings.checkForAppUpdates, isFalse);
  });

  test('a television still starts dark when it does not follow the system', () {
    expect(newInstallSettings(isTvDevice: true).themeIsDark, isTrue);
    expect(newInstallSettings(isTvDevice: false).themeIsDark, isFalse);
  });
}
