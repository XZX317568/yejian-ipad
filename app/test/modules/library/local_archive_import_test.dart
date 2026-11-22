// ignore_for_file: depend_on_referenced_packages
import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:typed_data';

import 'package:archive/archive.dart';
import 'package:bot_toast/bot_toast.dart';
import 'package:cross_file/cross_file.dart';
import 'package:file_picker/file_picker.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:isar_community/isar.dart';
import 'package:material_ui/material_ui.dart';
import 'package:mangayomi/models/chapter.dart';
import 'package:mangayomi/models/manga.dart';
import 'package:mangayomi/models/settings.dart';
import 'package:mangayomi/models/source.dart';
import 'package:mangayomi/modules/library/providers/file_scanner.dart';
import 'package:mangayomi/modules/library/providers/local_archive.dart';
import 'package:mangayomi/repositories/db.dart' as database;
import 'package:mangayomi/repositories/db_write_queue.dart';
import 'package:mangayomi/router/router.dart';
import 'package:mangayomi/services/chapter_cache.dart';
import 'package:mangayomi/services/crash_report.dart';
import 'package:mangayomi/services/get_chapter_pages.dart';
import 'package:mangayomi/services/local_folders.dart';
import 'package:mangayomi/src/rust/api/epub.dart';
import 'package:mangayomi/src/rust/frb_generated.dart';
import 'package:mangayomi/utils/extensions/others.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider_platform_interface/path_provider_platform_interface.dart';

final _image = base64Decode(
  'iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAI0lEQVR4nGP8//8/AymAiSTVDKMaiANMRKqDg1ENxACSQwkAVW0DHeN02ZEAAAAASUVORK5CYII=',
);

final class _PickedFile extends PlatformFile {
  _PickedFile(this.uri);
  @override
  final Uri uri;
  @override
  String get name => uri.pathSegments.last;
  @override
  XFile get xFile => XFile(path!);
  @override
  int? lengthSync() => path == null ? null : File(path!).lengthSync();
  @override
  Future<int> length() => File(path!).length();
  @override
  Future<Uint8List> readAsBytes() => File(path!).readAsBytes();
  @override
  Stream<Uint8List> readAsByteStream() =>
      File(path!).openRead().map(Uint8List.fromList);
}

class _Picker extends FilePickerPlatform {
  _Picker(this.files);
  final List<PlatformFile> files;
  @override
  Future<List<PlatformFile>> pickFiles({
    String? dialogTitle,
    String? initialDirectory,
    FileType type = FileType.any,
    List<String>? allowedExtensions,
    Function(FilePickerStatus)? onFileLoading,
    int compressionQuality = 0,
    AndroidOptions androidOptions = const AndroidOptions(),
    DarwinOptions darwinOptions = const DarwinOptions(),
    WindowsOptions windowsOptions = const WindowsOptions(),
    LinuxOptions linuxOptions = const LinuxOptions(),
    WebOptions webOptions = const WebOptions(),
  }) async => files;
}

class _Paths extends PathProviderPlatform {
  _Paths(this.path);
  final String path;
  @override
  Future<String?> getApplicationDocumentsPath() async => path;
}

class _EpubDecoder extends Fake implements RustLibApi {
  @override
  Future<EpubNovel> crateApiEpubParseEpubFromPath({
    required String epubPath,
    required bool fullData,
  }) async {
    if (p.basename(epubPath) == 'broken.epub') {
      throw const FormatException('Test damaged EPUB');
    }
    return const EpubNovel(
      name: 'Book',
      chapters: [EpubChapter(name: 'Chapter', content: '', path: '1.xhtml')],
      images: [],
      stylesheets: [],
    );
  }
}

void main() {
  late Directory root;
  late Isar db;
  late ProviderContainer container;
  late FilePickerPlatform originalPicker;
  late PathProviderPlatform originalPaths;
  final pageCaches = <String>{};

  setUpAll(() async {
    TestWidgetsFlutterBinding.ensureInitialized();
    // Use the already resolved dependency, never download a native test binary.
    final configFile = File('.dart_tool/package_config.json').absolute;
    final config =
        jsonDecode(await configFile.readAsString()) as Map<String, dynamic>;
    final library = (config['packages'] as List)
        .cast<Map<String, dynamic>>()
        .singleWhere((p) => p['name'] == 'isar_community_flutter_libs');
    final package = Directory.fromUri(
      configFile.uri.resolve(library['rootUri'] as String),
    );
    final binary = p.join(
      package.path,
      Platform.isWindows ? 'windows' : 'linux',
      Platform.isWindows ? 'libisar.dll' : 'libisar.so',
    );
    await Isar.initializeIsarCore(libraries: {Abi.current(): binary});
    RustLib.initMock(api: _EpubDecoder());
  });
  setUp(() async {
    root = await Directory.systemTemp.createTemp('local-import-test-');
    CrashReports.resetForTest();
    originalPicker = FilePickerPlatform.instance;
    originalPaths = PathProviderPlatform.instance;
    PathProviderPlatform.instance = _Paths(root.path);
    ChapterCache().customCacheDir = Directory(
      p.join(root.path, 'page-list-cache'),
    );
    db = await Isar.open(
      [MangaSchema, ChapterSchema, SettingsSchema, SourceSchema],
      directory: root.path,
      name: 'import',
    );
    database.isar = db;
    db.writeTxnSync(() => db.settings.putSync(Settings()));
    container = ProviderContainer(retry: (_, _) => null);
  });
  tearDown(() async {
    container.dispose();
    await dbWriteQueue.run(() {});
    await db.close();
    FilePickerPlatform.instance = originalPicker;
    PathProviderPlatform.instance = originalPaths;
    ChapterCache().customCacheDir = null;
    for (final path in pageCaches) {
      final directory = Directory(path);
      if (await directory.exists()) await directory.delete(recursive: true);
    }
    pageCaches.clear();
    await root.delete(recursive: true);
  });

  Future<File> makeArchive(String name) async {
    final file = File(p.join(root.path, name));
    return file.writeAsBytes(
      ZipEncoder().encode(
        Archive()
          ..addFile(ArchiveFile('1.png', _image.length, _image))
          ..addFile(ArchiveFile('2.png', _image.length, _image)),
      ),
    );
  }

  testWidgets(
    'picker imports good files on both sides of a broken archive and a missing path',
    (tester) async {
      await tester.pumpWidget(
        MaterialApp(
          navigatorKey: navigatorKey,
          builder: BotToastInit(),
          navigatorObservers: [BotToastNavigatorObserver()],
          home: const SizedBox(),
        ),
      );
      await tester.runAsync(() async {
        final first = await makeArchive('first.cbz');
        final broken = await File(p.join(root.path, 'broken.zip'))
            .writeAsString('not a zip');
        final last = await makeArchive('last.zip');
        FilePickerPlatform.instance = _Picker([
          _PickedFile(first.uri),
          _PickedFile(broken.uri),
          _PickedFile(Uri.parse('content://test/missing.cbz')),
          _PickedFile(last.uri),
        ]);
        expect(
          await container.read(
            importArchivesFromFileProvider(
              null,
              itemType: ItemType.manga,
              init: true,
            ).future,
          ),
          '',
        );
        expect(db.mangas.countSync(), 1);
        expect(CrashReports.latest!.error, contains('broken.zip'));
        expect(CrashReports.latest!.error, contains('missing.cbz'));
        final chapters = db.chapters.where().findAllSync();
        expect(chapters.map((e) => e.name).toSet(), {'first', 'last'});
        final chapter = chapters.first..manga.loadSync();
        final pages = await container.read(
          getChapterPagesProvider(chapter: chapter).future,
        );
        expect(pages.archiveImages, [null, null]);
        expect(pages.pageUrls.length, 2);
        expect(pages.isLocaleList, [true, true]);
        for (final page in pages.uChapDataPreload) {
          expect(page.archivePage, isNotNull);
          pageCaches.add(p.dirname(page.archivePage!.cachePath));
          expect(await page.getImageBytes, _image);
        }
      });
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 250));
      expect(
        find.textContaining('Import failed for 2 file(s); imported 2.'),
        findsWidgets,
      );
      BotToast.cleanAll();
      await tester.pumpAndSettle();
    },
  );

  testWidgets('all-bad selection leaves no empty title or chapter', (
    tester,
  ) async {
    await tester.pumpWidget(
      MaterialApp(
        navigatorKey: navigatorKey,
        builder: BotToastInit(),
        navigatorObservers: [BotToastNavigatorObserver()],
        home: const SizedBox(),
      ),
    );
    await tester.runAsync(() async {
      final broken = await File(p.join(root.path, 'bad.cbz'))
          .writeAsString('bad');
      FilePickerPlatform.instance = _Picker([_PickedFile(broken.uri)]);
      await container.read(
        importArchivesFromFileProvider(
          null,
          itemType: ItemType.manga,
          init: true,
        ).future,
      );
      expect(db.mangas.countSync(), 0);
      expect(db.chapters.countSync(), 0);
    });
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 250));
    expect(
      find.textContaining('Import failed for 1 file(s); imported 0.'),
      findsWidgets,
    );
    BotToast.cleanAll();
    await tester.pumpAndSettle();
  });

  testWidgets(
    'directory scanner isolates a parser failure and persists later chapters',
    (tester) async {
      await tester.pumpWidget(
        MaterialApp(
          navigatorKey: navigatorKey,
          builder: BotToastInit(),
          navigatorObservers: [BotToastNavigatorObserver()],
          home: const SizedBox(),
        ),
      );
      await tester.runAsync(() async {
        final defaultFolder = (await getDefaultLocalFolder())!;
        final title = Directory(p.join(defaultFolder.path!, 'Novel'))
          ..createSync(recursive: true);
        for (final name in ['before.epub', 'broken.epub', 'after.epub']) {
          File(p.join(title.path, name))
              .writeAsStringSync('synthetic decoder fixture');
        }
        await container.read(scanLocalLibraryProvider.future);
        final chapters = db.chapters.where().findAllSync();
        expect(chapters.length, 2);
        expect(chapters.map((e) => p.posix.basename(e.archivePath!)).toSet(), {
          'before.epub',
          'after.epub',
        });
        expect(chapters.every((e) => e.mangaId != null), true);
      });
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 250));
      expect(
        find.textContaining('Import failed for 1 file(s); imported 2.'),
        findsWidgets,
      );
      BotToast.cleanAll();
      await tester.pumpAndSettle();
    },
  );
}
