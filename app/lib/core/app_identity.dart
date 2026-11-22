import 'package:mangayomi/models/settings.dart';

/// The product identity of this fork.
///
/// The Dart package and the Rust library keep their upstream names so the
/// existing import and bridge code stays intact. The values that a person, an
/// operating system or a store sees are separate from that internal naming:
/// this app is 页间, installs as `app.yejian.reader`, and keeps its reader data
/// in its own folder.
abstract final class AppIdentity {
  static const productName = '页间';
  static const englishName = 'Yejian';
  static const applicationId = 'app.yejian.reader';

  /// The visible folder for downloads, backups and logs. On Apple platforms
  /// and desktop the database lives underneath this folder too; on Android it
  /// stays in the app's private storage. Both are separate from Mangayomi's
  /// Documents/Mangayomi directory.
  static const storageFolderName = 'Yejian';

  /// Links created for 页间 use its own scheme, so an upstream Mangayomi
  /// install cannot claim them.
  static const linkScheme = 'yejian';

  /// OAuth callbacks in the tracker and sync modules still request the
  /// upstream scheme. The mobile manifests keep accepting it so those logins
  /// continue to work; moving them to [linkScheme] is tracked separately and
  /// removes the remaining coexistence conflict.
  static const legacyLinkScheme = 'mangayomi';

  static const copyright = 'Copyright (C) 2023 Mangayomi contributors';
}

/// Values a brand-new Settings row starts with.
///
/// These are applied only while creating the singleton row. A row that already
/// exists is left exactly as it is, including preferences that happen to match
/// Mangayomi's old defaults.
Settings newInstallSettings({required bool isTvDevice}) => Settings()
  // The library opens as the full 3D shelf. A series saved in an older build
  // keeps its own grid/list choice; this only sets the first run.
  ..displayType = DisplayType.shelf
  ..themeIsDark = isTvDevice
  ..followSystemTheme = true
  ..useFloatingNavigationBar = true
  ..enableDiscordRpc = false
  ..checkForAppUpdates = false
  ..hideItems = const ['/AnimeLibrary', '/NovelLibrary', '/trackerLibrary'];
